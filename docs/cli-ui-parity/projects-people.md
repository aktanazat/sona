# Projects, People, Companies: UI action → CLI coverage inventory

> Summary table with an MCP column and current statuses: [README.md](README.md). It also covers what landed after this inventory: project fields on the project page, project updates with health, the 30-day bin, milestone reorder, and the Milestones and Project updates tables. This file keeps the per-button Swift evidence.

Repo: /Users/aktan/Developer/sona-confirmed-voice-mean (branch work/people-projects). Read-only inventory, no builds.
Swift paths relative to /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/. Bridge = method name in /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/native_bridge/dispatch.rs.
CLI = `sona-core work <verb>` (/Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/query/external/tracker.rs:81-171, args in tracker/catalog.rs:83-215) or the top-level read flags (/Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/cli.rs:127-304: `--people`, `--person-catch-up`, `--person-changes`, `--person-stats`, `--projects`, `--project`, `--loops`, `--roster`, `--loop-resolve`).

Data homes: "history DB" = planning tables reached via `planning::history` (catalog.rs:373-376). "meeting store" = encrypted store behind `MeetingSessionManager` (commands/people.rs, workflow_engine/people.rs); the CLI opens it read-only-mounted via `MeetingSessionManager::new(..).reading_only()` (lib.rs:2365, meeting/session.rs:696) and still writes people through it (catalog.rs:905-916, 960-972).

## Projects

| # | UI action | Swift trigger (file:line) | Bridge method | CLI | Class | Data / note |
|---|---|---|---|---|---|---|
| P1 | List projects (page load, refresh) | ProjectsStore.swift:537 | planning_projects | `work projects [--all]`; `--projects` | cli-exists | history DB |
| P2 | Filter list by status / overdue / assigned to me / clear; filter chips | ProjectsView.swift:177-195, 225 | none (client filter over P1) | `work projects` + `work list --project X --due overdue` / `--mine` | cli-exists | client-side only |
| P3 | Search projects by title/description | ProjectsView.swift:91-92 | none (client filter) | `work projects` (no text filter) | cli-exists | trivial client filter; CLI has no `--query` on projects |
| P4 | Open project (row tap, Return key, a11y action) | ProjectsView.swift:277-279, 341, 1120 | planning_project (ProjectsStore.swift:553) | `--project UUID`; `work milestones P`; `work list --project P` | cli-exists | history DB |
| P5 | Refresh project detail (↻) | ProjectsView.swift:419-420 | planning_project | `--project UUID` | cli-exists | |
| P6 | Close project (Esc), arrow-key cursor | ProjectsView.swift:470, 275-276 | none | n/a | ui-only-runtime | navigation only, no data |
| P7 | New project (button, `c` key) → sheet Create (title + description) | ProjectsView.swift:171, 282, 388 → save 1598 | planning_create_project (ProjectsStore.swift:851) | `work project-save NAME --details ...` | cli-exists | history DB |
| P8 | Change project status: context menu "Move to …" | ProjectsView.swift:338, 347, 380 | planning_update_project (ProjectsStore.swift:895) | `work project-save NAME --status backlog\|planned\|active\|paused\|completed\|canceled` | cli-exists | the project page status menu sends planning_project_save (ProjectPage.swift:184) |
| P9 | Change project status: drag card to board column | ProjectsView.swift:382, 393-396, 1070 | planning_update_project | `work project-save --status` | cli-exists | |
| P10 | Change project status: detail header picker | ProjectsView.swift:499-501 | planning_update_project | `work project-save --status` | cli-exists | |
| P11 | Edit description / priority / lead / members / start and target with resolution / labels / colour / icon / links | ProjectPage.swift:184-428, ProjectsView.swift:496 | planning_project_save | `work project-save --details --priority --lead --members --start --start-resolution --target --target-resolution --labels --hue --icon --link` | cli-exists | rename (`--project`) and archive (`work project-archive [--restore]`) stay CLI-only; the bin is `work project-delete [--restore]` and `work projects --trashed` on both sides |
| P12 | Add milestone (button) → sheet Create (title, due) | ProjectsView.swift:630 → 1600 | planning_create_milestone (ProjectsStore.swift:862) | `work milestone-save PROJECT NAME --target DAY` | cli-exists | CLI target is a day; UI picks date+time (ProjectsView.swift:1632-1634) |
| P13 | Change milestone status (picker) | ProjectsView.swift:660-661 | planning_update_milestone (ProjectsStore.swift:909) | `work milestone-save P NAME --status not-started\|in-progress\|done\|canceled` | cli-exists | Swift `PlanningWorkStatus` (ProjectsStore.swift:16-19) = CLI `MilestoneState` (catalog.rs:45-51) |
| P14 | Add task in project (button, `c` key in detail) → sheet Create (title, details, milestone, owner, due) | ProjectsView.swift:705, 465-468 → 1602 | planning_create_task (ProjectsStore.swift:873) | `work create TITLE --project --milestone --owner me\|PERSON --due --details` | cli-exists | history DB |
| P15 | Change task status in project (picker) | ProjectsView.swift:735-736 | planning_update_task (ProjectsStore.swift:923) | `work update S-N --status`, `work done` | cli-exists | |
| P16 | Owner filter chip / "Show all" in task list | ProjectsView.swift:561-563, 700 | none (client) | `work list --project P --owner X` | cli-exists | |
| P17 | Open person from task avatar | ProjectsView.swift:781 | none (navigation) | n/a | ui-only-runtime | navigation |
| P18 | Link source to project / milestone / task (link buttons → sheet "Link source") | ProjectsView.swift:674, 768, 836 → sheet 1308 → save 1448 | planning_link_source (ProjectsStore.swift:931) | none (`work done --evidence URL` only attaches a URL to a task) | missing | history DB; candidate sources read from meeting store (meeting_list, meeting_loops) and notes (scratchpad_list), ProjectsStore.swift:624-642 |
| P19 | Browse candidate sources in link sheet (type picker, meeting drill-in, Load more) | ProjectsView.swift:1260-1265, 1276, 1340, 1378 | meeting_list, meeting_loops, scratchpad_list | `work meetings`, `--loops`, `--notes` | cli-exists | reads only; needed as inputs for P18 |
| P20 | View linked sources on project | ProjectsView.swift:835 (Sources section) | planning_project | `--project UUID` ("live owner and source availability", cli.rs:259) | cli-exists | [INFERENCE] field-level parity with the Sources section not verified |
| P21 | Review links sheet: pick sources, "Review selected sources" | ProjectsView.swift:414 → PlannerReviewView.swift:195, 251 → 487 | planning_generate_suggestions (ProjectsStore.swift:691) | none | missing | history DB + model call; source reads meeting store/notes |
| P22 | Review links: load / refresh suggestions | PlannerReviewView.swift:88, 113, 341-343, 351 | planning_suggestions (ProjectsStore.swift:671) | none | missing | history DB |
| P23 | Review links: "Link evidence" (accept) / "Dismiss" (decline) | PlannerReviewView.swift:409-415 | planning_accept_suggestion / planning_decline_suggestion (ProjectsStore.swift:823-833) | none | missing | history DB |
| P24 | Nighttime review: "Allow nighttime review" / "Save selected sources" / "Turn off" | PlannerReviewView.swift:270, 294 → 330 | planning_quiet_review_set (ProjectsStore.swift:740) | none | ui-only-deliberate | consent grant for unattended model reading of chosen sources |
| P25 | Nighttime review state read | PlannerReviewView.swift:89, 281, 285 | planning_quiet_review_grants (ProjectsStore.swift:713) | none | ui-only-deliberate | read of the consent grants in P24 |
| P26 | Nighttime review run / cancel | QuietReviewScheduler.swift:150, 109 (no button) | planning_quiet_review_run / planning_quiet_review_cancel (ProjectsStore.swift:766, 781) | none | ui-only-runtime | scheduler needs 22:00–06:00, 10 min idle input, awake Mac, Apple Intelligence (ProjectsStore.swift:797-809) |
| P27 | Plan with Sona: Send goal / revise | ProjectsView.swift:161, 168 → PlannerAgentView.swift:443 → PlannerAgentStore.swift:265-268 | planning_prepare / planning_revise | none | missing | model call over selected sources; no write until P28 |
| P28 | Plan with Sona: "Create project" from draft | PlannerAgentView.swift:404 → PlannerAgentStore.swift:335 | planning_accept | none (manual equivalent: project-save + milestone-save + create) | missing | history DB |
| P29 | Plan with Sona: edit draft (remove milestone/task/commitment/citation, Keep previous, Use revision, Discard) | PlannerAgentView.swift:218, 243, 297, 361, 393-395, 401 | none (client draft) | n/a | missing | belongs with P27/P28; draft lives in app memory |
| P30 | Plan with Sona: voice input | PlannerAgentView.swift:430-432 | (recorder) | n/a | ui-only-runtime | needs live microphone |
| P31 | Talk to Sona about project | ProjectsView.swift:417 | chat surface (not planning) | n/a | out of surface | chat inventory owns it |
| P32 | Planner calendar (load range, refresh, retry) | PlannerCalendarView.swift:49, 70, 81 | planning_calendar (ProjectsStore.swift:609) | partial: `work milestones P`, `work list --due ...` | missing | history DB; no date-range view of milestones+tasks across projects |
| P33 | Overdue tile on Overview → Projects filtered | OverviewView.swift:346 | none (client) | `work list --due overdue` | cli-exists | |

## People

| # | UI action | Swift trigger (file:line) | Bridge method | CLI | Class | Data / note |
|---|---|---|---|---|---|---|
| H1 | List people (load, Retry) | PeopleScreen.swift:104, 500; PeopleStore.swift:168 | people_list | `work people [--query]`; `--people NAME` | cli-exists | meeting store; `work people` refuses while the conversations fence is closed (catalog.rs:443-447) |
| H2 | Open person page | PeopleScreen.swift:121, 1349 | person_detail (PeopleStore.swift:181) | partial: `work people --query`, `--person-stats ID`, `--person-catch-up ID` | missing | meeting store; no single CLI read returns the page (summary, meeting links with confidence, documents, contact, evidence) |
| H3 | Create person | no direct button; only via speaker sheet "New person" (PeopleScreen.swift:1707, 1853) or split to new person (1121) | voice_identify_speaker / person_split | `work person-save NAME` | cli-exists | CLI-only as a direct create (catalog.rs:917-920 `person_profile_create`) |
| H4 | Rename (menu → inline field commit) | PeopleScreen.swift:564 → PersonOverview.swift:23 → PeopleStore.swift:285-290 | person_rename | `work person-save NEW --person OLD` | cli-exists | CLI keeps the old name as an alias (catalog.rs:170); [INFERENCE] whether `person_rename` does the same not checked |
| H5 | Edit aliases / role / organization / email / notes | no UI control | none | `work person-save --aliases --role --org --email --notes` | cli-exists | CLI-only today |
| H6 | Regenerate summary | PeopleScreen.swift:565 → PeopleStore.swift:474 | person_summary_regenerate | none | missing | meeting store + model call |
| H7 | Import document | PeopleScreen.swift:566 → Shell.swift:132 (opens Settings › Importing) | documents surface | n/a | out of surface | documents/import inventory |
| H8 | Delete imported document | PeopleScreen.swift:852 → Shell.swift:133 | documents surface | n/a | out of surface | documents inventory |
| H9 | Choose photo (Contacts, file, saved original, public image URL) → Save photo | PeopleScreen.swift:567 → PersonPhotoChooser.swift:291-294, 305, 352; reads 192, write 267 | person_avatar_source, person_avatar_set | none | missing | meeting store. Contacts source needs Contacts permission and the "allow request without a login" toggle (PersonPhotoChooser.swift:303) is a privacy gate → those two parts are ui-only-deliberate; a photo from a file path is headless-able |
| H10 | Remove photo | PeopleScreen.swift:568 → PeopleStore.swift:319 | person_avatar_set (png null) | none | missing | meeting store |
| H11 | Retry photo (portrait context menu) | PersonPortrait.swift:71 | person_avatar | n/a | ui-only-runtime | re-render only |
| H12 | Attach / replace contact (menu, "Add contact") | PeopleScreen.swift:569, PersonOverview.swift:36 → PersonContactPicker.swift:23-35 → PeopleStore.swift:353 | person_contact_set | none | ui-only-deliberate | CNContactPicker + Contacts privacy: "the contact picked there is the only one that ever reaches this person" (PeopleStore.swift:331-332) |
| H13 | Remove contact | PeopleScreen.swift:571 → PeopleStore.swift:325-327, 353 | person_contact_set (contact null) | none | missing | meeting store; removal needs no Contacts access |
| H14 | Merge into another person | PeopleScreen.swift:574 → sheet 1047-1050 → PeopleStore.swift:386 | person_merge | `work person-merge A --into B` | cli-exists | CLI also repoints tracker tasks (catalog.rs:978) |
| H15 | Merge voice conflict: keep this / keep other voice / cancel | PeopleScreen.swift:528-535 → PeopleStore.swift:369-371 | person_merge (voiceProfileResolution) | none: CLI sends `voice_profile_resolution: None` and tells the user to use the People page (catalog.rs:970-975) | missing | meeting store; choosing which saved voice survives deletes biometric samples, so a CLI flag needs the same explicit choice |
| H16 | Split person (move meetings / aliases / emails / documents to new or existing person) | PeopleScreen.swift:576 → sheet 1102-1105 → PeopleStore.swift:429 | person_split | none | missing | meeting store |
| H17 | Link a meeting manually (picker, search, Show more, select) | PeopleScreen.swift:577-578, 1246, 1292-1301, 1254 → PeopleStore.swift:253, 446-460 | meeting_list, link_add_manual | none (reads: `work meetings`) | missing | meeting store |
| H18 | Confirm suggested meeting link | PeopleScreen.swift:807 → PeopleStore.swift:442-443 | link_confirm | none | missing | meeting store |
| H19 | Dismiss suggested link / Unlink confirmed meeting | PeopleScreen.swift:810, 814 → 958 → PeopleStore.swift:446-447 | link_remove | none | missing | meeting store |
| H20 | Remove voice profile | PeopleScreen.swift:582 → 948 → PeopleStore.swift:492 | voice_remove_profile | none | ui-only-deliberate | biometric privacy control; only deletes data, so it could be exposed if the owner wants |
| H21 | Delete person | PeopleScreen.swift:583 → 938 → PeopleStore.swift:404 | person_delete | none | missing | meeting store; irreversible, needs confirmation flag |
| H22 | First met: read | PersonFirstMetView.swift:131 | person_first_met | none | missing | meeting store |
| H23 | First met: "Yes" / "No" on introduction | PersonFirstMetView.swift:79-81 → 119 | person_first_met_decide | none | missing | meeting store |
| H24 | First met: play introduction / open meeting | PersonFirstMetView.swift:92-93, 61 | (meeting audio) | n/a | ui-only-runtime | audio playback / navigation |
| H25 | Person history / catch-up / what changed (Compare since) / subjects / profile; Refresh; Show earlier | PersonContinuityView.swift:58-63, 82, 129, 144, 261 → PersonMemoryStore.swift:54 | person_memory | partial: `--person-catch-up ID`, `--person-changes ID --since-utc-ms`, `--person-stats ID` | cli-exists | meeting store; [INFERENCE] subjects/profile tabs not checked against those flags |
| H26 | Person Work tab (commitments by meeting/project, owned tasks) | PersonContinuityView.swift:324, 361, 381 → PersonWorkStore.swift:116-119 | planning_person_work, work_list | owned tasks: `work list --owner PERSON`; commitments: `--loops` | cli-exists | partial: no single read groups a person's meeting commitments with their project |
| H27 | Open loops inbox on People list | PeopleScreen.swift:358 → PeopleStore.swift:221 | open_loops_inbox | `--loops [--mine\|--waiting]` | cli-exists | meeting store |
| H28 | Play "each time it came up" reel | PeopleScreen.swift:416-420 | conversation_reel_for_loop | n/a | ui-only-runtime | audio playback |
| H29 | Meeting people context (who was in a meeting, prior meetings count) | PeopleScreen.swift:1472 → PeopleStore.swift:271 | meeting_people_context | partial: `--roster MEETING_ID`, `work meeting ID` | cli-exists | meeting store; roster prints call-listed names, not the linked people with counts |
| H30 | Back to list, open company from person header | PeopleScreen.swift:482, 1322; PersonOverview.swift:63 | none / organization_detail | see C2 | ui-only-runtime | navigation |

## Speaker assignment (meeting page section and sheet)

| # | UI action | Swift trigger (file:line) | Bridge method | CLI | Class | Data / note |
|---|---|---|---|---|---|---|
| S1 | Load speakers and who is unresolved | PeopleScreen.swift:1557, 1635-1637 → PeopleStore.swift:667-669 | voice_identity_status, meeting_get | partial: `--transcript ID`, `work meeting ID --transcript` show speaker labels | missing | meeting store; no CLI read of unresolved speakers or suggestions |
| S2 | Confirm voice suggestion | PeopleScreen.swift:1508, 1613 → PeopleStore.swift:820-855 | voice_identify_speaker (label existing) or meeting_speaker_rename (own voice) | none | missing | meeting store |
| S3 | "Not them" / "Not me", "Label speaker", "Correct" (open sheet) | PeopleScreen.swift:1509-1510, 1521, 1541 | none (opens sheet) | n/a | ui-only-runtime | sheet navigation; the writes are S4/S6 |
| S4 | Sheet Save: name speaker as existing person or New person, plus same-voice meetings | PeopleScreen.swift:1706-1733, 1757, 1827 → 1850-1856 → PeopleStore.swift:872 | voice_identify_speaker (label / correctTo, create) | none | missing | meeting store; works on stored segments, no live audio needed |
| S5 | "Name this voice automatically in later meetings" checkbox on save | PeopleScreen.swift:1737 → PeopleStore.swift:893 | voice_enroll_profile (consentVersion 1) | none | ui-only-deliberate | voice-profile enrollment consent |
| S6 | Mark as unknown (request, confirm, cancel) | PeopleScreen.swift:1818-1824 → PeopleStore.swift:810-818 | voice_identify_speaker (markUnknown) | none | missing | meeting store; deletes the speaker's kept samples |
| S7 | Undo voice-assigned name | PeopleScreen.swift:1537, 1669 → PeopleStore.swift:766 | meeting_speaker_rename | none | missing | meeting store |
| S8 | Play speaker clip in sheet | PeopleScreen.swift:1786 | (meeting audio) | n/a | ui-only-runtime | audio playback |
| S9 | Not now / close sheet, Retry people | PeopleScreen.swift:1823, 1661, 1683 | people_list | `work people` | cli-exists | |

## Companies

| # | UI action | Swift trigger (file:line) | Bridge method | CLI | Class | Data / note |
|---|---|---|---|---|---|---|
| C1 | Companies list on People page (Retry) | PeopleScreen.swift:170-190, 180 → PeopleStore.swift:209 | companies_list | none | missing | meeting store (workflow_engine/people.rs:65-68) |
| C2 | Open company page (row, person header) | PeopleScreen.swift:190; PersonOverview.swift:63 → PeopleStore.swift:197 | organization_detail | none | missing | meeting store (workflow_engine/people.rs:55) |
| C3 | Open person from company page | PeopleScreen.swift:1349 | person_detail | see H2 | missing | |

## Gaps in one place

Missing, history DB: P18 link source, P21-P23 review-link suggestions, P27-P29 plan with Sona, P32 calendar range.
Missing, meeting store: H2 person page read, H6 regenerate summary, H9-H10 photo set/remove (file source), H13 remove contact, H15 merge voice choice, H16 split, H17-H19 meeting links, H21 delete person, H22-H23 first met, S1-S2, S4, S6-S7 speaker naming, C1-C3 companies.
Deliberate: P24-P25 nighttime review consent, H12 contact attach, H20 voice profile removal, S5 voice enrollment, plus the Contacts and public-URL parts of H9.
Runtime: P26 nighttime scheduler, P30 voice input, playback (H24, H28, S8), navigation rows.
CLI-only (no UI): project rename and archive, milestone rename/details/target/delete, project update edit, person create/aliases/role/org/email/notes.
