# Tasks, Triage, Today/Home: UI action → CLI inventory

> Summary table with an MCP column and current statuses: [README.md](README.md). It also covers what landed after this inventory: comments and threads, sub-tasks and parents, attachments, the milestone and repeat pickers on the task page, copy actions, palette task search, the filter builder with operators, and cycle settings, rename, rollover and start today. This file keeps the per-button Swift evidence.

Repo: /Users/aktan/Developer/sona-confirmed-voice-mean (branch work/people-projects). Read-only inventory, built from code only.

Bridge: /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/native_bridge/dispatch.rs. CLI: /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/query/external/tracker.rs (`sona-core work <verb>`) and /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/query/external.rs (top-level requests).

Classes: `cli-exists`, `missing`, `ui-only-deliberate`, `ui-only-runtime`.

For each `missing` row, the "Data" column says where the data lives: history DB (planning tables), meeting store (encrypted; opened in the CLI through `MeetingSessionManager::reading_only`), settings, or live runtime.

Note on CLI writes: CLI writes go through `agents::create_as` / WriteArgs `--agent`. Field changes made by an agent wait for the user's accept in Sona unless agent changes are allowed (tracker.rs WriteArgs doc). The UI writes as `Me`.

## Tasks screen: loading, tabs, views, filters, display

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Open Tasks / Retry load | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:99,134; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:640-647 | work_list, work_counts, work_views, work_cycles | `work list`, `work counts`, `work views`, `work cycles` | cli-exists | |
| Sidebar / tab badges (I owe count, Waiting count) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:272-273 | work_counts | `work counts` | cli-exists | |
| Tab "I owe" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:272 | work_list | `work mine` / `work list --mine` | cli-exists | |
| Tab "Waiting on" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:273 | work_list | `work list --waiting` | cli-exists | |
| Tab "Cycle N" (with progress) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:275; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/CycleView.swift:94 | work_list, work_cycle | `work cycle current` / `work cycle N`, `work list --cycle current` | cli-exists | |
| Tab "All" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:277 | work_list | `work list` | cli-exists | |
| Saved view tab | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:279,314 | work_list (view query) | `work views` then `work list` with the view's filters | cli-exists | |
| "This cycle" toggle on I owe | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:211 | none (client filter) | `work list --mine --cycle current` | cli-exists | |
| "Show all my work" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:181 | none (clears client toggle) | `work list --mine` | cli-exists | |
| Context menu "Delete view" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:280-281,296-301; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:417-422 | work_view_delete | `work view-delete` | cli-exists | |
| Save view (All tab, name popover) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:345,379-386; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:396-404 | work_view_save | `work view-save NAME` + filter flags + `--layout --group-by --order-by` | cli-exists | |
| Save edits to a view | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:355,389-393 | work_view_save | `work view-save` (existing view) | cli-exists | |
| Revert view edits | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:353 | none | n/a | ui-only-runtime | only drops the window's unsaved draft (`viewDrafts`) |
| Filter: search text | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:142 | work_list | `work list --search` | cli-exists | |
| Filter: status | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:158 | work_list | `--status` | cli-exists | |
| Filter: priority | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:167 | work_list | `--priority` | cli-exists | |
| Filter: owner / direction (mine, waiting, unowned, person) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:187,255 | work_list | `--mine --waiting --unowned --owner` | cli-exists | |
| Filter: project / no project | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:200,206 | work_list | `--project NAME\|none` | cli-exists | |
| Filter: labels | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:216 | work_list | `--label` | cli-exists | |
| Filter: due window | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:226 | work_list | `--due` / `--due-by` | cli-exists | |
| Filter: cycle | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:235 | work_list | `--cycle` | cli-exists | |
| Filter: include snoozed | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:242 | work_list | `--snoozed` | cli-exists | |
| Filter chip ×, "Clear", Clear filters button | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskFilters.swift:318,343; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:196 | none | n/a (run `work list` without the flag) | ui-only-runtime | window-only query state |
| Open filter panel (F), display panel (⇧V) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:516-519,536-539; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:220,232 | none | n/a | ui-only-runtime | opens a popover |
| Layout list/board (button, ⌘B) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:228; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:499-500; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/DisplayOptionsView.swift:23 | work_view_save (when on a view) | `work view-save --layout` | cli-exists | on built-in tabs it is window state only |
| Display: grouping, ordering | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/DisplayOptionsView.swift:27,39 | work_view_save | `work view-save --group-by --order-by` | cli-exists | |
| Display: show empty groups | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/DisplayOptionsView.swift:53 | work_view_save (`WorkDisplay.showEmptyGroups`, WorkTypes.swift:454) | MISSING | missing | history DB (saved view display). view-save has no flag for it |
| Display: shown properties | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/DisplayOptionsView.swift:70,104-107 | work_view_save (`WorkDisplay.properties`) | MISSING | missing | history DB (saved view). On built-in tabs it is window-only (`myProperties`) |
| Collapse/expand a group | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:370,390-393 | none | n/a | ui-only-runtime | window state |
| Group header "+" (new task prefilled with group's fields) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:254-263,371 | work_create | `work create TITLE --status --priority --owner --project --due` | cli-exists | |
| Reschedule overdue (Overdue header button, ⇧D, palette) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:372-373; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:513-515; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:339 | work_bulk_update (dueAtMs) | `work list --mine --due overdue`, then `work update S-1 S-2 … --due DAY` (or `work say`) | cli-exists | two steps in the CLI |
| Drag task(s) onto a board column / group | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:514,527-533 | work_update / work_bulk_update (group patch: status, priority, owner, project, due) | `work update … --status/--priority/--owner/--project/--due` | cli-exists | |
| Row click / Return (open task) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:425,428; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:93-96 | work_item | `work show S-N` | cli-exists | |
| Space peek | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskPeek.swift:20-21 | none (row in memory) | `work show` | cli-exists | |
| Selection (X, ⌘A, Select a11y action), j/k/arrows move, Esc | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:268,540-543; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:429; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/BulkActionBar.swift:19 | none | n/a (the CLI takes several task refs per verb) | ui-only-runtime | window selection |
| Undo (⌘Z, toast) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:491-494; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:476-493 | inverse of last change: work_update/work_bulk_update, work_archive, work_restore, work_delegate, work_time_block_remove | no `undo`; inverse verbs `update`, `archive`, `restore` exist | ui-only-runtime | the undo stack lives in the window's WorkStore memory. Delegate and unbook inverses are also missing, see below |

## Per-task actions (context menu, keys, palette, bulk bar, detail properties)

Entry points for every row in this section: context menu WorkActionMenu (/Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:426,446-451), detail ⋯ menu (/Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:31), bulk bar pickers (/Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/BulkActionBar.swift:26), the palette, and keys (/Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:551-565, perform 614-660).

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Status picker (S) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:57; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:764 | work_update / work_bulk_update | `work update --status`, `work done [--evidence]` | cli-exists | |
| Priority picker (P) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:61; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:769 | work_update / work_bulk_update | `--priority` | cli-exists | |
| Owner picker (A): me, person, no owner | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:65,145-160; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:776 | work_update / work_bulk_update | `--owner me\|nobody\|NAME` | cli-exists | |
| Assign to me (I) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:614-660 | work_bulk_update | `work update --owner me` | cli-exists | |
| Due picker (D): typed date, Today, Tomorrow, Next week, In two weeks, No due date | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:69-73,162-181; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:793 | work_update / work_bulk_update | `--due DAY\|none` | cli-exists | the CLI takes a day, not the UI's preset words |
| Labels picker (L), toggle on/off | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:76-81,300-311; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:856 | work_bulk_update (labelIds) | `--add-label --remove-label --labels` | cli-exists | |
| New label typed in a picker / composer chip | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:191-195; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskComposer.swift:106; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/ParsedChips.swift:125; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:361-367,471 | work_label_save | `work label-save --label NAME --hue` | cli-exists | no UI renames, recolors or deletes labels. WorkStore.deleteLabel (378) has no UI caller, but the CLI has `label-delete` |
| Project picker (⇧P) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:84,200-214; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:848 | work_update / work_bulk_update | `--project NAME\|none` | cli-exists | the task page now also has a milestone picker (TaskDetailView.swift:1724-1747, `WorkPatch.milestoneId`); the CLI has `--milestone` |
| Cycle picker (⇧C): current, next, none | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:99,242-254; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:818 | work_update / work_bulk_update (cycle) | `work update --cycle` | cli-exists | |
| Rename (E) inline / detail title submit | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:171; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:679; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:150,172 | work_update | `work update --title` | cli-exists | |
| Edit details (on leaving field) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:196,210-215 | work_update | `work update --details` | cli-exists | |
| Snooze picker (H): typed date, Tomorrow 9:00, In 3 days, Next week, In a month, Pick a day | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:91,216-240 | work_bulk_update (snoozedUntilMs) | `work snooze --until DAY` / `work update --snooze-until` | cli-exists | the CLI takes a day |
| Snooze "Later today" (a time of day) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:223-228 | work_bulk_update | MISSING | missing | history DB. `--until` takes a day only, so a time today cannot be given |
| Archive (⌘⌫, bulk bar) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/BulkActionBar.swift:33; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:139,685-689 | work_archive | `work archive` | cli-exists | |
| Restore (only through undo of archive) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:145,484-485 | work_restore | `work restore` | cli-exists | |
| Delegate to agent (⇧⌘;): Claude Code, Codex, OMP, Grok, No agent; copies the hand-off prompt | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:505-507; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkPickers.swift:95,256-272; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:783; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:350,695-703 | work_delegate | MISSING (`work claim` only lets the calling agent take the task itself; tracker.rs:2116-2171) | missing | history DB (doer). Copying the prompt to the clipboard is runtime-only |
| Book time (B): calendar hour for the task | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:562; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:834; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:165-171 | work_time_block | MISSING (CLI only reads `time_block`, tracker.rs:1167) | missing | history DB plus the system calendar (`platform_calendar`, /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/planning/work/blocks.rs:10,103). A CLI write would need calendar access in the CLI process |
| Unbook (undo of a booking) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:495-500 | work_time_block_remove | MISSING | missing | history DB plus system calendar (blocks.rs:121) |
| Repeats on an existing task | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:1556,1778 | work_update (`recurrence`) | `work update --repeat WORDS\|none` | cli-exists | the task page can now change a repeat after creation |
| Nudge (R) → sheet "Mail" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:326,354-385 | work_nudge_mail, then work_nudged | MISSING | ui-only-runtime | opens a Mail compose window; nothing is sent |
| Nudge sheet "Copy" (copies the draft, records the nudge) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:325,387-391 | work_nudged | MISSING | missing | history DB (`work_nudged` records the nudge time, follow.rs:1045-1048). The draft text is built in Swift, so the CLI has no draft either |
| Nudge sheet "Edit" draft, close | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:276,324 | none | n/a | ui-only-runtime | local text |
| Nudge sheet "Done", "Snooze" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:321-322,398-400 | work_update | `work done`, `work snooze` | cli-exists | |
| Accept proposal (Y), Accept all (⇧Y), Reject (N) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/ProposalViews.swift:18,93,143,146; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:305-339 | work_proposals_accept, work_proposals_reject | none | ui-only-deliberate | the human approval step on agents' changes (same gate as work_agent_changes) |
| Add relation: blocks / blocked by / related | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:531-533,560-570 | work_relation_add | `work relate S-1 blocks\|blocked-by\|related S-2` | cli-exists | |
| Remove relation | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:642 | work_relation_remove | `work unrelate` | cli-exists | |
| Open a related task | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:662 | work_item | `work show` | cli-exists | |
| Detail: prev/next (K/J, chevrons), Esc back | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:66,104,106 | none | n/a | ui-only-runtime | navigation |
| Detail: Retry | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:700 | work_item | `work show` | cli-exists | |
| Detail: evidence, suggestions, activity (read) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:121-128,272,673 | work_item | `work show` (relations, evidence; tracker.rs:1257-1261) | cli-exists | |
| Detail: evidence "Open" / source meeting row | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:324,449-450,870 | none (navigation) | `work meeting ID` / `meeting` to read it | ui-only-runtime | opens another screen |
| Detail: play the moment's audio | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:360,405 | meeting audio playback | n/a | ui-only-runtime | needs the audio output device |

## Creating tasks: composer, quick entry, ramble

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| New task (C, toolbar, empty state) → composer → Create task (⌘Return) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:548-550; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:186,201,239; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskComposer.swift:40,123,130,247-251; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:432-451 | work_create | `work create TITLE --details --status --priority --owner --due --project --labels --repeat` | cli-exists | |
| Typed words read as fields (owner, due, priority, project, labels, "every monday") with chips | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskComposer.swift:251,282,296; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/ParsedChips.swift:23; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:466-467 | work_parse | MISSING (`work create` keeps the title as typed; tracker.rs:1993-2033) | missing | no stored data. The parser is in the core and the CLI does not call it on create |
| Composer Cancel | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskComposer.swift:120 | none | n/a | ui-only-runtime | |
| Quick entry panel (global shortcut `work_quick_entry`) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/QuickEntry.swift:41-55,225; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EssentialsView.swift:31 | work_create | `work create` | cli-exists | the floating panel itself is runtime-only |
| Quick entry keeps the page it was pressed over as the task's link | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/QuickEntry.swift:117-132,325; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:446-449 | work_capture | MISSING (create has no link/source option) | missing | history DB (task plus link). The live app reads the link from the front browser; a CLI version would take a URL argument |
| Quick entry words read as a change (Return applies; ⌥Return makes a task) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/QuickEntry.swift:79-110,229 | work_command, then apply | `work say "…" [--dry-run]` | cli-exists | |
| Spoken ramble → Triage (shortcut `work_ramble`), filed card | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/QuickEntry.swift:152-156,394; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EssentialsView.swift:32 | event `work-ramble-filed` | n/a | ui-only-runtime | needs the microphone and recorder |
| Dictated change heard → panel | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/QuickEntry.swift:114-123 | event `work-command-heard` | `work say` for typed text | ui-only-runtime | needs the microphone |

## Ask, watch, palette

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Palette question | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Panels.swift:1540; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:533-534 | work_ask | `work ask` | cli-exists | |
| Answer lines → Triage tasks (⌘Return) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Panels.swift:1371-1380; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:540-548 | work_take_lines | `work ask … --take` | cli-exists | |
| Palette command row apply | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Panels.swift:1432,1539; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:514-525 | work_command | `work say` | cli-exists | |
| Watch a phrase (palette, person page) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Panels.swift:1434; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkAskViews.swift:103 | work_watch_save | `work watch [--person]` | cli-exists | |
| Unwatch | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Panels.swift:1435; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkAskViews.swift:145 | work_watch_remove | `work unwatch` | cli-exists | |
| Watches list | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:563-564 | work_watches | `work watches` | cli-exists | |
| Watch hit "Make task" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkAskViews.swift:141; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:606-611 | work_watch_task | MISSING | missing | history DB (/Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/planning/work/watch.rs:356) |
| Watch hit open / play | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkAskViews.swift:117,130 | none / audio | n/a | ui-only-runtime | navigation, audio device |
| Palette answer play | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Panels.swift:1382-1393 | audio | n/a | ui-only-runtime | audio device |

## Triage

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Open Triage / Retry | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TriageScreen.swift:51,85 | work_list | `work list --status triage` | cli-exists | |
| Groups by source meeting | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TriageScreen.swift:117-133 | work_list | `work list --status triage --meeting ID` | cli-exists | |
| Source splits (meetings, dictation, agents, people), Tab/⇧Tab | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TriageScreen.swift:135-140; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkKeyboard.swift:482-485 | none (client filter on row source) | MISSING (no source filter in the CLI) | missing | history DB (item source) |
| Accept (1), Decline (2), Duplicate (3), Snooze (H): row buttons and bar | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TriageScreen.swift:186-188,206,214-217; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:212-258 | work_triage_accept, work_triage_decline, work_mark_duplicate, work_bulk_update | `work accept [--ignore-suggestions]`, `work decline`, `work duplicate --of`, `work snooze` | cli-exists | |
| Suggestion chip (reason, Use, Dismiss) and the task page's Suggested lines (Use, Dismiss) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TriageScreen.swift:293,317; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TriageSuggestions.swift:3-23; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TaskDetailView.swift:587; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:468-472 | work_suggestion_accept, work_suggestion_dismiss | `work triage-suggestions [TASK]`, `work suggestion-accept ID`, `work suggestion-dismiss ID` | cli-exists | history DB (`planning_dismissed_hints`) |
| Agents' proposals group | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/ProposalViews.swift:18,93,143,146 | work_proposals_accept / work_proposals_reject | none | ui-only-deliberate | human approval gate |
| "Tasks" (g t) from an empty Triage | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TriageScreen.swift:91 | none | n/a | ui-only-runtime | navigation |

## Today: morning plan

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Read today's focus | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:56 | work_day | `work list`/`show` print `focus` (tracker.rs:1155,1883-1884,4735) | cli-exists | read-only |
| Plan contents: due today/overdue, promises in Triage, meetings today | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:131-153 | work_list (computed in Swift) | `work list --mine --due …`, `work list --status triage`, `upcoming` | cli-exists | |
| Stale tasks (untouched 30 days, 5 asked) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:131-153 | work_list (computed in Swift) | MISSING (no "untouched for N days" filter) | missing | history DB |
| Open plan (button, auto after 6:00) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/MorningPlanView.swift:40; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:217-231 | none | n/a | ui-only-runtime | |
| Pick up to 3 focus tasks (Space / click) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/MorningPlanView.swift:180,187; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:252-260 | none until start | see next row | ui-only-runtime | window state |
| Start the day (Return): accept picked promises, save focus | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/MorningPlanView.swift:53; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:262-276 | work_triage_accept + work_day_plan | accept: `work accept`; focus: MISSING | missing | history DB (`items::plan_day`, /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/planning/work.rs:1522-1537) |
| Stale "Keep" (K): the task still stands | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/MorningPlanView.swift:268; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:287-289; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:153-158 | work_keep | MISSING | missing | history DB (`items::keep`, work.rs:1491-1508) |
| Stale "Archive" (X) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/MorningPlanView.swift:269 | work_archive | `work archive` | cli-exists | |
| Skip the rest (Esc) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/MorningPlanView.swift:271 | none | n/a | ui-only-runtime | |

## Home: today strip, evening, rest of Home

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Today strip counts (meetings today, processing, unnamed speakers, untracked promises, owed) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:92-120; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewStore.swift:274 | home_today | MISSING as one read (parts: `upcoming`, `loops`, `work counts`) | missing | meeting store + history DB + calendar detection (/Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/commands/meeting.rs:53-56) |
| Evening: kept / slipped / tomorrow lists | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningView.swift:20-24; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:157-185 | work_list (computed in Swift) | `work list --mine` with due/status filters | cli-exists | no "done today" filter; it must be read from the timestamps |
| Slipped → Tomorrow (→) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningView.swift:135; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:322-330 | work_update | `work update --due tomorrow` | cli-exists | |
| Slipped → This week (W, Friday) | same | work_update | `work update --due DAY` | cli-exists | |
| Slipped → Drop (X, canceled) | same | work_update | `work update --status canceled` | cli-exists | |
| Evening j/k move | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/Rituals.swift:372-430 | none | n/a | ui-only-runtime | |
| Evening card read | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningStore.swift:83 | home_evening | MISSING | missing | meeting store + history DB (/Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/commands/evening.rs:11-16) |
| "Sum up today" / "Rewrite" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningView.swift:163,175,184; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningStore.swift:56 | home_evening_write | MISSING | missing | meeting store. Runs `MeetingSessionManager::evening_now` "on this Mac" (evening.rs:18-33), so a CLI version needs that write path, not `reading_only` |
| Not filed: "File" / "Not this" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningView.swift:211,213; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningStore.swift:66 | filing_decide | MISSING | missing | meeting store (`decide_filing`, evening.rs:37-49) |
| Filing evidence: play voice / open | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/EveningView.swift:254,267,285 | none / audio | n/a | ui-only-runtime | audio device, navigation |
| Needs you: open loops | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewStore.swift:246 | open_loops_inbox | `loops` (ExternalRequest) | cli-exists | |
| Learning suggestion "Yes" / "No thanks" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:624,628; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewStore.swift:172,394 | learning_suggestions, learning_decide | MISSING | missing | meeting store (/Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/commands/learning.rs:40-52) |
| Update row "View" / "Dismiss" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:648,650 | check_for_updates | n/a | ui-only-runtime | app updater |
| Upcoming list / "Join" | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:713; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewStore.swift:403 | meeting_upcoming_events | `upcoming` (read); Join opens the call link | cli-exists (read) / ui-only-runtime (join) | |
| Series: always record | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:756 | meeting_series_always_record_set | none | ui-only-deliberate | consent to record without asking |
| Series: template | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:771 | meeting_series_template_set | MISSING | missing | meeting settings (MeetingSettingsStore 1023-1047); belongs to the meetings surface |
| Series: in digest | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:775 | meeting_series_digest_set | MISSING | missing | meeting settings; belongs to the meetings surface |
| Capture mode chip / Edit modes | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:424,435,462,471; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewStore.swift:187,388 | get_modes, set_active_mode | MISSING | missing | settings (modes) |
| Stats card: period, paging, Retry | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:869,896,1018,1025; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewStore.swift:340,347,356 | get_history_trend, meeting_trend, get_history_usage_stats | MISSING (`stats` returns `corpus_stats`, a different total; external.rs:1666-1668) | missing | history DB + meeting store |
| Widgets: commitments, overdue projects, people (tap → screen) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:274,290,305,346,368 | reads (open_loops_inbox, planning) | `loops`, `projects`, `people` | cli-exists | taps are navigation |
| Recent conversations/notes, See all, Retry | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:1394-1395,1436-1438,1467,1522; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewStore.swift:297,306,321 | meeting_list, scratchpad_recent, workflow_runs | `meetings`, `notes`; workflow runs: MISSING | cli-exists / missing (workflow runs) | workflow-run storage not checked (workflows surface) |
| New note | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:1356 | scratchpad (notes surface) | out of this surface | — | |
| Start/Stop/Cancel dictation | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/CaptureScreen.swift:131-171 | hud_toggle_recording, finish_recording, cancel_operation | `sona --toggle-transcription`, `sona --cancel`, `sona --toggle-post-process` (sent to the running app) | cli-exists | |
| Record conversation, Record screen, Import audio | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/OverviewView.swift:1361; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/CaptureScreen.swift:131-171 | recorder commands | n/a | ui-only-runtime | microphone, screen capture, file picker |

## Meeting brief and reminders (work_* methods)

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Meeting brief sheet (people, owed both ways, talking points, last time) | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:849 | work_brief | MISSING | missing | history DB + meeting store + calendar runtime (/Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/planning/work/follow.rs:908-911) |
| Add talking point | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:808 | work_talking_point_add | MISSING | missing | history DB + meeting store (follow.rs:946-950) |
| Talking point done / undone | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:830 | work_talking_point_done | MISSING | missing | history DB (follow.rs:992) |
| Brief: toggle a task done | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:719 | work_update | `work done` / `work update --status` | cli-exists | |
| Brief: open meeting, play | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:602,752,766 | none / audio | n/a | ui-only-runtime | |
| Reminder notification opened | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/FollowThrough.swift:134-153 | event `work-reminder-opened` | n/a | ui-only-runtime | system notification |

## Cycles

| UI action | Swift file:line | Bridge method | CLI command | Class | Data / note |
|---|---|---|---|---|---|
| Cycle progress | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/CycleView.swift:94; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:200-201 | work_cycle | `work cycle N` | cli-exists | |
| Cycle settings: length, starts on, cooldown, how many ahead; rename; roll over; start today | /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/TasksScreen.swift:217; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/CycleView.swift:160,208-217; /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/WorkStore.swift:293-325 | work_cycle_settings_save, work_cycle_save, work_cycle_rollover, work_cycle_start_today | `work cycle-settings`, `work cycle-save --name`, `work cycle-rollover`, `work cycle-start-today` | cli-exists | |

## Gaps to build, ranked by usefulness

1. Today's focus write (work_day_plan) and "still stands" (work_keep). Both history DB; small verbs.
2. Hand a task to a named agent or take it back (work_delegate). History DB.
3. (done: `work cycle-settings`, `cycle-save`, `cycle-rollover`, `cycle-start-today`)
4. Task with a source link (work_capture, as `create --link URL`) and words read as fields on create (work_parse). History DB.
5. Triage source filter (meetings, dictation, agents, people), a "Later today" snooze time, and a stale filter. History DB.
6. Saved view display: show-empty-groups and shown properties. History DB.
7. Record a nudge (work_nudged) and print the nudge draft. History DB; the draft is built in Swift today.
8. Watch hit → task (work_watch_task). History DB.
9. Book time / unbook (work_time_block, work_time_block_remove). History DB plus the system calendar; needs calendar access in the CLI process.
10. Meeting brief and talking points (work_brief, work_talking_point_add, work_talking_point_done). History DB + meeting store.
11. Home reads and evening writes: home_today, home_evening, home_evening_write, filing_decide, learning_suggestions/decide, usage/trend stats, set_active_mode, series template/digest. Meeting store and settings; home_evening_write needs a writable meeting store, not `reading_only`.

The CLI can do some things the UI cannot: set a cycle's days, delete or recolor a label, list archived tasks, export tasks as Markdown, attach an evidence link on done, and let an agent claim a task.
