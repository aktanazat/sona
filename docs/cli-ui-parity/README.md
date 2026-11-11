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
| Typed words read as fields (owner, due, "every monday") with chips | `work_parse` | — | — | missing |
| Quick entry keeps the page it was pressed over as the task's link | `work_capture` | `work create TITLE --source-url URL [--source-title T] [--dry-run]` | `sona_work_capture` *(opt-in)* | both |
| Quick entry words read as a change | `work_command` | `work say "…" [--dry-run]` | `sona_work_say` | both |
| Reschedule overdue (header button, ⇧D) | `work_bulk_update` | `work list --due overdue`, then `work update … --due DAY` | `sona_work_list`, `sona_work_update` | both |
| Drag tasks onto a board column or group | `work_update`, `work_bulk_update` | `work update --status/--priority/--owner/--project/--due` | `sona_work_update` | both |
| Palette: find a task, project, person or label by name or number | none (matches loaded rows) | `work list --search`, `work projects`, `work people --query`, `work labels` | `sona_work_list` (`search`), `sona_project_list`, `sona_person_list`, `sona_work_labels` *(opt-in)* | both |
| Copy ID / Copy link / Copy as Markdown | none (pasteboard, `TaskCopy`) | `work show S-N [--json]`; `work export --md DIR` writes Markdown | `sona_work_get` | window-bound: pasteboard; the text itself is readable |
| Export matching tasks as Markdown files | — | `work export --md DIR` + list filters | — | CLI only |
| Selection, peek, collapse groups, open filter/display panels | none | — (verbs take several task refs) | — | window-bound: window state |
| Undo (⌘Z, toast) | inverse of the last call | — (inverse verbs: `work update`, `work archive`, `work restore`) | — | window-bound: the undo stack lives in the window |
| Spoken ramble to Triage; dictated change | events `work-ramble-filed`, `work-command-heard` | typed: `work say` | `sona_work_say` | window-bound: microphone |

## Task page

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Open a task: quote, evidence, suggestions, activity, relations | `work_item` | `work show S-N` | `sona_work_get` | both |
| Status picker | `work_update` | `work update --status`, `work done` | `sona_work_update`, `sona_work_done` | both |
| Mark done with an evidence link | `work_update` | `work done --evidence URL` | `sona_work_done` (`evidence`) | CLI only (the evidence link) |
| Agent starts a task as itself | — | `work claim` | `sona_work_claim` | CLI only |
| Priority, owner, assign to me, due, labels, project, milestone, cycle | `work_update`, `work_bulk_update` | `work update --priority --owner --due --labels --add-label --remove-label --project --milestone --cycle` | `sona_work_update` | both |
| Rename, edit details | `work_update` | `work update --title`, `--details` | `sona_work_update` | both |
| Repeats | `work_update` (`recurrence`) | `work update --repeat WORDS\|none` | `sona_work_update` (`repeat`) | both |
| Snooze to a day | `work_bulk_update` | `work snooze --until DAY`, `work update --snooze-until DAY` | `sona_work_update` (`snooze_until`) | both |
| Snooze "Later today" (a time) | `work_bulk_update` | `work snooze TASK --until later-today`, `work update --snooze-until later-today` | `sona_work_update` (`snooze_until`) | both |
| Archive | `work_archive` | `work archive` | `sona_work_archive` *(opt-in)* | both |
| Restore (undo of archive) | `work_restore` | `work restore` | `sona_work_restore` *(opt-in)* | both |
| Relations: blocks, blocked by, related; remove | `work_relation_add`, `work_relation_remove` | `work relate S-1 blocks\|blocked-by\|related S-2`, `work unrelate` | `sona_work_relate`, `sona_work_unrelate` *(opt-in)* | both |
| Set or remove the parent | `work_update` (`parentId`) | `work update --parent TASK\|none` | `sona_work_update` (`parent`) | both |
| Add sub-tasks | `work_subtask_create` | `work subtask PARENT TITLE… [--owner --priority]` | `sona_subtask_create` | both |
| Read comment threads | `work_comments` | `work comments TASK` | `sona_comment_list` | both |
| Comment, or reply in a thread | `work_comment_add` | `work comment add [--reply-to]` | `sona_comment_save` | both |
| Edit own comment | `work_comment_edit` | `work comment edit`, `work comment-edit` | `sona_comment_save` (`id`) | both |
| Delete own comment | `work_comment_delete` | `work comment delete`, `work comment-delete` | `sona_comment_delete` *(opt-in)* | both |
| Resolve or reopen a thread | `work_comment_resolve` | `work comment-resolve [--reopen]` | `sona_comment_resolve` | both |
| List attachments | `work_attachments` | `work attachments TASK` | `sona_task_attachments` | both |
| Attach a file | `work_attachment_attach_file` | `work attach TASK PATH [--title --subtitle]` | `sona_task_attach` (`path`) | both |
| Attach a link | `work_attachment_attach_link` | `work attach TASK URL [--title --subtitle]` | `sona_task_attach` (`url`) | both |
| Rename an attachment | `work_attachment_rename` | `work attachment-rename TASK ID --title` | `sona_work_attachment_rename` *(opt-in)* | both |
| Detach | `work_attachment_detach` | `work detach TASK ID` | `sona_task_detach` *(opt-in)* | both |
| Open an attachment, Copy path, Copy link | none | — | — | window-bound: Finder, pasteboard |
| Delegate to a named agent (copies the hand-off prompt) | `work_delegate` | `work delegate TASK --to AGENT`, `--take-back`; prints the prompt | `sona_work_delegate` *(opt-in)* | both |
| Book time / unbook | `work_time_block`, `work_time_block_remove` | — | — | missing |
| Nudge: record it (Copy) | `work_nudged` | — | — | missing |
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
| Decline | `work_triage_decline` | `work decline` | `sona_work_triage` (`decline`) | both |
| Duplicate of | `work_mark_duplicate` | `work duplicate TASK --of TASK` | `sona_work_triage` (`duplicate`) | both |
| Snooze | `work_bulk_update` | `work snooze --until DAY` | `sona_work_update` (`snooze_until`) | both |
| Ask every conversation; take answer lines into Triage | `work_ask`, `work_take_lines` | `work ask "Q"`, `work ask "Q" --take=1,3` | `sona_work_ask`, `sona_work_take` | both |
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
| Priority, lead, members, start and target with resolution, labels, colour, icon, links, description | `planning_project_save` | `work project-save NAME --priority --lead --members --start --start-resolution --target --target-resolution --labels --hue --icon --link --details` | `sona_project_save` | both |
| Rename a project | — (no UI caller sends `title`) | `work project-save NEW --project OLD` | `sona_project_save` | CLI only |
| Put away (archive) or bring back | — | `work project-archive [--restore]` | `sona_project_archive` *(opt-in)* | CLI only |
| Move to the bin, restore from the bin | `planning_project_save` (`trashed`) | `work project-delete [--restore]` | `sona_project_delete` *(opt-in)* | both |
| Add a task in the project | `planning_create_task` | `work create TITLE --project P --milestone M --owner --due` | `sona_work_create` | both |
| Change a task's status in the project | `planning_update_task` | `work update --status`, `work done` | `sona_work_update`, `sona_work_done` | both |
| Link a meeting, note or loop as a source | `planning_link_source` | — | — | missing |
| Review links: generate, list, accept, dismiss suggestions | `planning_generate_suggestions`, `planning_suggestions`, `planning_accept_suggestion`, `planning_decline_suggestion` | — | — | missing |
| Plan with Sona: draft, revise, create from draft | `planning_prepare`, `planning_revise`, `planning_accept` | — (by hand: `work project-save`, `work milestone-save`, `work create`) | — | missing |
| Planner calendar (milestones and tasks by date range) | `planning_calendar` | — (parts: `work milestones P`, `work list --due`) | — | missing |
| Nighttime review: allow, choose sources, turn off | `planning_quiet_review_set`, `planning_quiet_review_grants` | — | — | UI-only by design: consent for unattended reading |
| Nighttime review run or cancel | `planning_quiet_review_run`, `planning_quiet_review_cancel` | — | — | window-bound: the scheduler needs an idle, awake Mac |

## Milestones

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| List with target days, task counts, progress | `planning_project` | `work milestones P`, `work project P` | `sona_milestone_list`, `sona_project_detail` | both |
| Add a milestone | `planning_create_milestone` | `work milestone-save P NAME --target DAY` | `sona_milestone_save` | both |
| Change status | `planning_update_milestone` | `work milestone-save P NAME --status` | `sona_milestone_save` | both |
| Drag to reorder | `planning_milestone_reorder` | `work milestone-reorder P M1 M2 …` | `sona_milestone_reorder` | both |
| Rename, details, move the target | — | `work milestone-save P NEW --milestone OLD --details --target` | `sona_milestone_save` | CLI only |
| Delete (its tasks stay) | — | `work milestone-delete` | `sona_milestone_delete` *(opt-in)* | CLI only |

## Project updates

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Read the update feed | `planning_project_updates` | `work project-updates P [--limit --cursor]` | `sona_project_update_list` | both |
| Update due, health, latest update | `planning_project` | `work project P` | `sona_project_detail` | both |
| Draft from what moved | `planning_project_update_draft` | `work project-update-draft P` | — (`sona_project_update_save` without `body` posts the draft) | both |
| Post an update with health | `planning_project_update_add` | `work project-update P --health on-track\|at-risk\|off-track [--body]` | `sona_project_update_save` | both |
| Edit own update | `planning_project_update_edit` (no UI caller) | `work project-update-edit ID [--health --body]` | `sona_project_update_save` (`id`) | CLI only |
| Delete own update | `planning_project_update_delete` | `work project-update-delete ID` | `sona_project_update_delete` *(opt-in)* | both |

## Cycles

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Cycle list and lengths | `work_cycles` | `work cycles` | `sona_cycle_list` *(opt-in)* | both |
| Cycle tab progress graph | `work_cycle` | `work cycle current\|next\|N` | `sona_cycle_list` (`cycle`) *(opt-in)* | both |
| Cycle settings: length, start day, cooldown, how many ahead | `work_cycle_settings_save` | `work cycle-settings --weeks --start-weekday --cooldown --upcoming` | `sona_cycle_save` *(opt-in)* | both |
| Rename a cycle | `work_cycle_save` | `work cycle-save CYCLE --name` | `sona_cycle_save` (`name`) *(opt-in)* | both |
| Move a cycle's days | `work_cycle_save` (no UI caller sends days) | `work cycle-save CYCLE --start --end` | `sona_cycle_save` (`start`, `end`) *(opt-in)* | CLI only |
| Roll unfinished work forward | `work_cycle_rollover` | `work cycle-rollover CYCLE` | `sona_cycle_save` (`rollover`) *(opt-in)* | both |
| Start the next cycle today | `work_cycle_start_today` | `work cycle-start-today [CYCLE]` | `sona_cycle_save` (`start_today`) *(opt-in)* | both |
| Put a task in a cycle | `work_update` | `work update --cycle current\|next\|N\|none` | `sona_work_update` (`cycle`) | both |

## Views and filters

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Filter: search, status, priority, owner, project, label, due, cycle, snoozed | `work_list` | `work list --search --status --priority --owner --project --label --due --due-by --cycle --snoozed` | `sona_work_list` | both |
| Filter builder with operators (is, is not, any of, none of, includes all, before, after) | `work_list` (`query.filters`) | `work list --filter JSON` | `sona_work_list` (`filter`) | both |
| Saved view tabs | `work_views`, `work_list` | `work views`, `work list --view V` | `sona_work_views` *(opt-in)*, `sona_work_list` (`view`) | both |
| Save a view, save edits to it | `work_view_save` | `work view-save NAME [--view V] --filter --layout --group-by --order-by` + filter flags | `sona_work_view_save` *(opt-in)* | both |
| Delete a view | `work_view_delete` | `work view-delete` | `sona_work_view_delete` *(opt-in)* | both |
| Display: show empty groups, shown properties | `work_view_save` | — | — | missing |
| Revert unsaved view edits, clear filter chips | none | — | — | window-bound: the window's draft |
| Labels list; new label from a picker | `work_labels`, `work_label_save` | `work labels`, `work label-save NAME` | `sona_work_labels`, `sona_work_label_save` *(opt-in)* | both |
| Rename, recolor, delete a label | — (no UI caller) | `work label-save NAME --label OLD --hue`, `work label-delete` | `sona_work_label_save`, `sona_work_label_delete` *(opt-in)* | CLI only |

## People

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| List people | `people_list` | `work people [--query]`, `sona --people NAME` | `sona_person_list`, `sona_people` | both |
| Person history, catch-up, what changed, stats | `person_memory` | `sona --person-catch-up ID`, `--person-changes ID --since-utc-ms`, `--person-stats ID` | `sona_person_catch_up`, `sona_person_changes`, `sona_person_stats` | both |
| Person's work and open loops | `planning_person_work`, `work_list`, `open_loops_inbox` | `work list --owner PERSON`, `sona --loops [--mine\|--waiting]` | `sona_work_list` (`owner`), `sona_action_items` | both |
| Person page in one read (summary, meeting links, documents, contact) | `person_detail` | — (parts above) | — | missing |
| Add a person directly | — (UI adds only through speaker naming or split) | `work person-save NAME` | `sona_person_save` | CLI only |
| Rename | `person_rename` | `work person-save NEW --person OLD` | `sona_person_save` | both |
| Other names, role, organization, email, notes | — | `work person-save NAME --aliases --role --org --email --notes` | `sona_person_save` | CLI only |
| Merge into another person | `person_merge` | `work person-merge A --into B` | `sona_person_merge` *(opt-in)* | both |
| Merge: choose which saved voice survives | `person_merge` (`voiceProfileResolution`) | — | — | missing |
| Split a person | `person_split` | — | — | missing |
| Delete a person | `person_delete` | — | — | missing |
| Regenerate summary | `person_summary_regenerate` | — | — | missing |
| Photo from a file; remove photo | `person_avatar_set` | — | — | missing |
| Photo from Contacts or a public image URL | `person_avatar_source`, `person_avatar_set` | — | — | UI-only by design: Contacts access and the no-login request toggle |
| Attach a contact | `person_contact_set` | — | — | UI-only by design: only the contact picked in the system picker reaches Sona |
| Remove a contact | `person_contact_set` (null) | — | — | missing |
| Link, confirm or remove a meeting link | `link_add_manual`, `link_confirm`, `link_remove` | — | — | missing |
| First met: read, yes/no | `person_first_met`, `person_first_met_decide` | — | — | missing |
| Name a speaker, confirm a suggestion, mark unknown, undo | `voice_identify_speaker`, `meeting_speaker_rename` | — | — | missing |
| Save a voice profile; remove one | `voice_enroll_profile`, `voice_remove_profile` | — | — | UI-only by design: biometric consent |
| Watch a phrase from the person page | `work_watch_save` | `work watch TOPIC --person P` | `sona_work_watch` *(opt-in)* | both |
| Companies list and page | `companies_list`, `organization_detail` | — | — | missing |
| Play "each time it came up" | `conversation_reel_for_loop` | — | — | window-bound: audio output |

## Meetings, notes, search

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| List meetings, time window | `meeting_list` | `work meetings [--since --limit]`, `sona --meetings --last --from --to` | `sona_meeting_list`, `sona_meetings` | both |
| Title search, status filter, folder chips | `meeting_list` | — | — | missing |
| Open a meeting: notes, ledger, transcript | `meeting_get` | `work meeting ID [--transcript]`, `sona --meeting ID`, `sona --transcript ID` | `sona_meeting_get`, `sona_meeting`, `sona_transcript` | both |
| Wrap up, write the follow-up | `meeting_wrap`, `meeting_follow_up_draft`, `work_follow_up` | `work follow-up MEETING` | `sona_work_follow_up` | both |
| Post the follow-up to a connection | `work_follow_up_post` | — | — | missing |
| Ledger loops: read, done, drop | `meeting_loops`, `meeting_loop_resolve` | `sona --loops`, `sona --loop-resolve LOOP_ID` | `sona_action_items`, `sona_loop_resolve` | both |
| Loop reopen, loop owner | `meeting_loop_reopen`, `meeting_loop_assign` | `work loop-reopen LOOP_ID`, `work loop-owner LOOP_ID --owner PERSON\|none` | `sona_meeting_loop_reopen`, `sona_meeting_loop_assign` *(opt-in)* | both |
| Live card Accept / Decline | `work_triage_accept`, `work_triage_decline` | `work accept`, `work decline` | `sona_work_triage` | both |
| Untracked action: Track, Dismiss, Restore | `planning_track_action`, `planning_dismiss_action`, `planning_restore_action` | `work action-track LOOP_ID --project P`, `work action-dismiss LOOP_ID`, `work action-restore LOOP_ID` | `sona_planning_track_action`, `sona_planning_dismiss_action`, `sona_planning_restore_action` *(opt-in)* | both |
| Who was in the call; place; bookmarks; heard plans | `meeting_call_name_status`, `conversation_place_review`, `meeting_heard_plans` | `sona --roster ID`, `--place ID`, `--bookmarks`, `--plans` | `sona_roster`, `sona_place`, `sona_bookmarks`, `sona_plans` | both |
| Upcoming meetings | `meeting_upcoming_events` | `sona --upcoming` | `sona_upcoming` | both |
| Rename a meeting | `meeting_title_set` | — | — | missing |
| Folders: create, rename, delete, add a meeting | `meeting_folder_create`, `meeting_folder_rename`, `meeting_folder_delete`, `meeting_folder_add_meeting` | — | — | missing |
| Delete, trash, restore, delete forever | `meeting_delete`, `meeting_trash_list`, `meeting_trash_restore`, `meeting_trash_delete_forever` | — | — | missing |
| Export Markdown/JSON, all as CSV, ledger HTML | `meeting_export`, `meeting_export_all_csv`, `produce_ledger_html` | — (closest: `sona --export-meeting ID --format`, `--export-meetings DIR`) | — | missing |
| Write the notes again, catch me up, rewrite with my notes | `meeting_artifacts_regenerate`, `meeting_catch_up`, `reenhance_meeting_with_notes` | — | — | missing |
| Speaker rename or merge, edit a line, transcript corrections | `meeting_speaker_rename`, `meeting_speaker_merge`, `meeting_segment_edit`, `transcript_correction_update` | — | — | missing |
| Meeting notes and my notes; action item done | `meeting_note_create`, `meeting_note_update`, `meeting_note_delete`, `save_meeting_user_notes`, `set_action_item_done` | — | — | missing |
| Ask about one meeting or one folder | `sona_query_pack` | — (`work ask` asks every conversation) | `sona_work_ask` (all conversations) | missing |
| Documents: add, list, delete | `doc_ingest`, `doc_list`, `doc_delete` | — | — | missing |
| Import a recording or transcript into the library | `meeting_import_recording`, `meeting_import_transcript` | — (`sona -f WAV` transcribes without saving) | — | missing |
| Share links: create, list, revoke | `cloud_share_create`, `cloud_share_list`, `cloud_share_revoke` | — | — | missing |
| Create a public browser share | `cloud_browser_share_create` | — | — | UI-only by design: publishes a link |
| Send notes to a connection | `connections_send_notes` | — | — | missing |
| Record, pause, stop a meeting; live help; bookmarks while live | `meeting_start`, `meeting_pause`, `meeting_stop`, `meeting_live_help`, `conversation_bookmark_create` | — | — | window-bound: live microphone and recorder |
| Recording consent panel; always record a series | `meeting_consent_panel_start`, `meeting_series_always_record_set` | — | — | UI-only by design: recording consent |
| Snapshots: delete, list | `meeting_snapshot_delete`, `meeting_snapshot_list` | — | — | missing |
| Automatic snapshots, on-screen text capture | `meeting_snapshot_automatic_set`, `meeting_snapshot_text_set` | — | — | UI-only by design: screen-capture consent |
| Heard plan: add to calendar | `meeting_heard_plan_add_to_calendar` | — | — | UI-only by design: writes to the owner's calendar |
| Heard plan: edit, not now | `meeting_heard_plan_edit`, `meeting_heard_plan_not_now` | — | — | missing |
| Notes: list, open, search, versions, diff, related | `scratchpad_list`, `scratchpad_get`, `scratchpad_versions` | `sona --notes`, `--note ID`, `--query T --scope notes`, `--note-versions ID`, `--note-diff ID`, `--note-context T` | `sona_notes`, `sona_note`, `sona_search`, `sona_note_versions`, `sona_note_diff`, `sona_note_context` | both |
| Notes: new, edit, pin | `scratchpad_create`, `scratchpad_update_body`, `scratchpad_set_pinned` | `work note-new TEXT`, `work note-edit ID --body`, `work note-pin ID [--unpin]` | `sona_scratchpad_create`, `sona_scratchpad_update_body`, `sona_scratchpad_set_pinned` *(opt-in)* | both |
| Notes: delete, restore a version | `scratchpad_delete`, `scratchpad_restore_version` | — | — | missing |
| Notes: link a meeting, person or note as a source | `scratchpad_link` | `work note-link ID --meeting\|--person\|--note ID` | `sona_scratchpad_link` *(opt-in)* | both |
| Notes: unlink a source, subjects, synthesis | `scratchpad_unlink`, `scratchpad_subject_create`, `scratchpad_synthesize` | — | — | missing |
| Palette corpus search; events | `sona_query_search`, `sona_query_events` | `sona --query T [--scope]`, `sona --events` | `sona_search`, `sona_events` | both |
| Said lines, on-screen text, in-meeting search | `meeting_said_search`, `meeting_snapshot_text_search`, `meeting_search` | — | — | missing |
| Dictation history: search, totals | `search_history_entries`, `get_history_stats` | `sona --query T --scope dictations`, `sona --stats` | `sona_search`, `sona_stats` | both |
| Dictation history: list, save, delete, process again, transcribe again | `get_history_entries`, `toggle_history_entry_saved`, `delete_history_entry`, `reprocess_history_entry`, `retry_history_entry_transcription` | — | — | missing |
| Evening digest settings; evening card; filing | `change_meeting_digest_enabled_setting`, `home_evening`, `home_evening_write`, `filing_decide` | — | — | missing |
| Recording and meeting retention | `update_recording_retention_period`, `meeting_retention_set` | — | — | UI-only by design: privacy retention |
| Play a turn, listen with context, receipts reel | `conversation_audio_clip`, `conversation_reel_for_loop` | — | — | window-bound: audio output |

## Chat (Talk to Sona)

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Send a turn, stop it, retry | `agent_panel_send_turn`, `agent_panel_cancel_turn` | — (`work ask` answers from local lines) | `sona_work_ask` (local lines) | missing |
| New chat, open a past chat | `agent_chat_new`, `agent_chat_open`, `agent_chat_history_list` | — | — | missing |
| Model and thinking effort | `agent_panel_select_model` | — | — | missing |
| Apply, undo or dismiss a proposed change or action | `agent_panel_apply_change`, `agent_panel_undo_change`, `agent_panel_apply_action`, `agent_panel_dismiss_action` | — (some actions by hand: `sona --loop-resolve`, `work person-save`) | — | missing |
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
| Use, download, delete, rescan a model | `set_active_model`, `download_model`, `delete_model`, `rescan_local_models` | — (`--model` applies to `-f` only) | — | missing |
| Dictation and sound toggles (push-to-talk, languages, sounds, mute, quiet speech, command mode, overlay) | `change_ptt_setting`, `change_dictation_languages_setting`, `change_audio_feedback_setting`, `change_mute_while_recording_setting`, `change_overlay_style_setting` | — | — | missing |
| Microphone and channel | `set_selected_microphone`, `set_selected_channel` | — | — | missing |
| Shortcuts: reset | `reset_binding` | — | — | missing |
| Shortcuts: record a new key | `start_handy_keys_recording`, `change_binding` | — | — | window-bound: live keyboard |
| Modes: new, edit, activate, reorder, delete | `upsert_mode`, `set_active_mode`, `reorder_modes`, `delete_mode` | — | — | missing |
| Mode rule from the front app or website | `capture_mode_activation_rule`, `capture_mode_website_activation_rule` | — | — | window-bound: reads the frontmost window |
| Cleanup provider, base URL, model | `set_post_process_provider`, `change_post_process_base_url_setting`, `change_post_process_model_setting` | — (`sona --toggle-post-process` toggles a run, not the setting) | — | missing |
| Local cleanup: download, use, remove | `download_local_cleanup`, `select_local_cleanup`, `remove_local_cleanup` | — | — | missing |
| Provider API keys | `set_provider_secret`, `delete_provider_secret`, `verify_stt_provider_secret` | — | — | UI-only by design: secrets |
| Cloud speech or cleanup consent | `accept_cloud_stt_provider_consent`, `accept_post_process_provider_consent` | — | — | UI-only by design: consent |
| Dictation prompts: add, edit, use, delete | `add_post_process_prompt`, `update_post_process_prompt`, `set_post_process_selected_prompt`, `delete_post_process_prompt` | — | — | missing |
| Saved prompts: save, run, delete | `saved_prompt_save`, `saved_prompt_run`, `saved_prompt_delete` | — | — | missing |
| Vocabulary, snippets, rewrites, CSV apply | `update_vocabulary_entries`, `set_snippets_enabled`, `reset_text_replacements`, `apply_vocabulary_csv`, `add_vocabulary_correction` | — | — | missing |
| Workflows: list, runs, enable | `workflows_list`, `workflow_runs`, `workflow_set_enabled` | — | — | missing |
| Meeting prep preferences | `meeting_prep_preferences_get`, `meeting_prep_preferences_set` | — | — | missing |
| Keyword trackers; series template and digest | `list_keyword_trackers`, `save_keyword_trackers`, `meeting_series_template_set`, `meeting_series_digest_set` | — | — | missing |
| Learn or forget my voice | `voice_user_learn`, `voice_user_forget` | — | — | UI-only by design: biometric consent |
| Web research key | `meeting_web_research_key_set`, `meeting_web_research_key_remove` | — | — | UI-only by design: secret |
| Agents: external access, mutations, autonomy, folders, permission rules, replies | `set_agent_bridge_master`, `change_work_agent_changes_setting`, `authorize_agent_bridge_project`, `create_agent_bridge_permission_rule`, `confirm_agent_bridge_reply` | — | — | UI-only by design: the gate the CLI itself sits behind |
| Agent panel pairing | `set_agent_panel_pairing`, `clear_agent_panel_pairing` | — | — | UI-only by design: token |
| Agent panel connection test | `agent_panel_test_connection` | — | — | missing |
| Connections: connect, allow sending, automatic sends, disconnect | `connections_save`, `connections_rule_save`, `connections_disconnect` | — | — | UI-only by design: token and consent to send |
| Connections: list, test, undo a send | `connections_snapshot`, `connections_test`, `connections_undo_send` | — | — | missing |
| Sync: set up, pair, approve a device, recover | `cloud_sync_bootstrap`, `cloud_sync_pairing_offer`, `cloud_sync_pairing_approve`, `cloud_sync_recover` | — | — | UI-only by design: vault keys |
| Sync: pause, resume, retry, resolve a conflict | `cloud_sync_pause`, `cloud_sync_resume`, `cloud_sync_retry`, `cloud_sync_conflict_resolve` | — | — | missing |
| About and debug: language, theme, update check, log level, debug mode | `change_app_language_setting`, `change_theme_setting`, `check_for_updates`, `set_log_level`, `change_debug_mode_setting` | — (`sona --debug` at launch) | — | missing |
| Open app data, logs, recordings folders | `open_app_data_dir`, `open_log_dir`, `open_recordings_folder` | — | — | window-bound: Finder |
| Launch at login; project root folder | `change_autostart_setting`, `change_dictation_project_root_setting` | — | — | UI-only by design: system login item and folder grant |

## Rest (Home, Today, rituals, brief, watches)

| UI action | core/bridge method | CLI command | MCP tool | status |
|---|---|---|---|---|
| Start, stop, cancel dictation | `hud_toggle_recording`, `finish_recording`, `cancel_operation` | `sona --toggle-transcription`, `sona --cancel`, `sona --toggle-post-process` | — | both |
| Transcribe a WAV headless | — | `sona -f WAV [--model --json --diagnose --repeat]` | — | CLI only |
| Export meetings' audio and speakers for analysis | — | `sona --export-meetings DIR` | — | CLI only |
| Record the screen; import audio from Finder | recorder sheet, file picker | — | — | window-bound: screen capture, file picker |
| Today strip (meetings today, unnamed speakers, owed) | `home_today` | — (parts: `sona --upcoming`, `sona --loops`, `work counts`) | — | missing |
| Morning plan: read focus | `work_day` | `work list`, `work show` print `focus` | `sona_work_list` | both |
| Morning plan: save up to 3 focus tasks | `work_day_plan` | `work focus TASK... [--day]`, `work focus --clear`; read with `work day [DAY]` | `sona_work_day_plan`, `sona_work_day` *(opt-in)* | both |
| Stale task: still stands (Keep) | `work_keep` | `work keep TASK...` | `sona_work_keep` *(opt-in)* | both |
| Stale tasks list (untouched 30 days) | `work_list` (computed in Swift) | `work list --stale` | `sona_work_list` (`stale`) | both |
| Evening: slipped to tomorrow, this week, drop | `work_update` | `work update --due`, `work update --status canceled` | `sona_work_update` | both |
| Person brief and talking points | `work_brief` (person), `work_talking_point_add`, `work_talking_point_done` | `work brief PERSON`, `work talking-point PERSON TEXT`, `work talking-point-done ID [--undo]` | `sona_work_brief`, `sona_work_talking_point_add`, `sona_work_talking_point_done` *(opt-in)* | both |
| Meeting brief for a calendar event | `work_brief` (event) | — | — | window-bound: calendar runtime |
| Watches: list, add, stop | `work_watches`, `work_watch_save`, `work_watch_remove` | `work watches`, `work watch`, `work unwatch` | `sona_work_watches`, `sona_work_watch` *(opt-in)* | both |
| Watch hit: make a task | `work_watch_task` | — | — | missing |
| Learning suggestions: yes, no thanks | `learning_suggestions`, `learning_decide` | — | — | missing |
| Stats trend; count words per app | `get_history_trend`, `meeting_trend`, `change_count_words_per_app_setting` | — (`sona --stats` gives totals) | `sona_stats` (totals) | missing |
| Capture mode chip | `get_modes`, `set_active_mode` | — | — | missing |

## Missing, ranked by how often an organizer agent would need it

1. Link sources to projects and review link suggestions (`planning_link_source`, `planning_suggestions` and friends).
2. Rename a meeting, folders, and asking about one meeting or one folder.
3. Notes: delete, restore a version, unlink a source, subjects, synthesis.
4. People: one-read person page, split, delete, meeting links, companies.
5. Words read as fields on create.
6. Watch hit to task, record a nudge, book time on the calendar.
7. Speaker naming and transcript corrections.
8. Settings writes (modes, models, vocabulary, prompts, workflows) and chat turns.

MCP gaps where the CLI already has the verb: `work snooze` (reachable as
`sona_work_update` `snooze_until`), `work export`, `work project-update-draft`
(draft without posting), and every top-level `sona` dictation or file flag.
Planning tools for cycles, labels, views, watches, deletes and archive are
opt-in only.

## Count of rows per status

| status | rows |
|---|---|
| both | 97 |
| CLI only | 17 |
| missing | 65 |
| UI-only by design | 23 |
| window-bound | 18 |
| total | 220 |

Rows are grouped: one row can cover several buttons that call the same
method. The inventories beside this file list every button.
