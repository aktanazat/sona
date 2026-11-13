# Sona UI to CLI inventory: meetings, notes, search, recordings history

> Summary table with an MCP column and current statuses: [README.md](README.md). This file keeps the per-button Swift evidence. The CLI verb is `work follow-up` (the rows below once said `work followup`, which does not exist).

Repo: /Users/aktan/Developer/sona-confirmed-voice-mean (branch work/people-projects). Read-only inventory; no builds run.

Swift paths are relative to /Users/aktan/Developer/sona-confirmed-voice-mean/macos/Sona/. Bridge names are the method strings in /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/native_bridge/dispatch.rs. `import_chosen_media_file` and `shutdown` are the only two methods in native_bridge/mod.rs.

CLI references:
- `sona-core --flag`: /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/cli.rs:127-308 and /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/query/external.rs.
- `sona-core work <verb>`: /Users/aktan/Developer/sona-confirmed-voice-mean/src-tauri/src/query/external/tracker.rs:81-170 and tracker/catalog.rs.
- Every CLI command needs Settings > Agents > External access. Writes also need External mutations.

Data location, for `missing` rows:
- MS: the meeting store. It is encrypted, and the CLI opens it with `MeetingSessionManager::reading_only`, so a write verb would need a writable open.
- NS: the notes (scratchpad) store. The CLI already reads it.
- HDB: the history DB. This holds dictation history entries and planning.
- SET: settings.
- RT: the live app runtime.
- CS: cloud-sync state and keys.

Clipboard-only, Finder-only and navigation-only actions are marked `ui-only-runtime`, because they need an app window, the pasteboard or Finder.

## 1. Meetings home and list (MeetingsScreen.swift, Shell.swift)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| list meetings, page Newer/Older | MeetingsScreen.swift:812, :816; MeetingsStore.swift:587 | meeting_list | `--meetings [--last N \| --from/--to]`; `work meetings --since --limit` | cli-exists |
| search meeting titles | MeetingsScreen.swift:240 → MeetingsStore.swift:587 | meeting_list (titleQuery) | MISSING (no title filter on `--meetings`; `--query --scope meetings` is a corpus search, not the same filter) | missing (MS) |
| status filter | MeetingsScreen.swift:244 | meeting_list (status) | MISSING | missing (MS) |
| time window picker | MeetingsScreen.swift:248 | meeting_list (window) | `--meetings --from/--to` | cli-exists |
| folder chips filter | MeetingsScreen.swift:267, :271 | meeting_list (folderId) | MISSING | missing (MS) |
| trend range menu | MeetingsScreen.swift:86, :141; MeetingsStore.swift:607 | meeting_trend | MISSING (`--stats` gives totals, not a trend) | missing (MS) |
| open a meeting row | MeetingsScreen.swift:605; MeetingsStore.swift:1061 | meeting_get | `--meeting ID`; `work meeting ID [--transcript]` | cli-exists |
| notice OK / dismiss | MeetingsScreen.swift:19, :25 | none | n/a | ui-only-runtime (window state) |
| ledger "Show in Finder" | MeetingsScreen.swift:32 | none (NSWorkspace) | n/a | ui-only-runtime (Finder) |
| "Export all as CSV…" | Shell.swift:265; MeetingsStore.swift:958 | meeting_export_all_csv | MISSING (`--export-meetings DIR` writes WAV, meeting.json and speakers.json, not CSV) | missing (MS) |
| import a recording or transcript (toolbar button) | Shell.swift:282 → MeetingLiveStore.swift:1554-1580 | meeting_import_recording / meeting_import_transcript | MISSING (`-f/--transcribe-file` gives a headless transcript with no history entry; the positional AUDIO only forwards to the running GUI import queue, lib.rs:2268) | missing (MS) |

## 2. Folders

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| "New folder", then save | MeetingsScreen.swift:277, :394-400; MeetingsStore.swift:774 | meeting_folder_create | MISSING | missing (MS) |
| folder menu "Rename", then save | MeetingsScreen.swift:303, :398-400; MeetingsStore.swift:782 | meeting_folder_rename | MISSING | missing (MS) |
| folder "Template and prompts", then save | MeetingsScreen.swift:307, :483; MeetingsStore.swift:801, :812 | meeting_folder_set_defaults, saved_prompt_list | MISSING | missing (MS; saved prompts in SET) |
| "Delete folder" and confirm | MeetingsScreen.swift:309, :339; MeetingsStore.swift:790 | meeting_folder_delete | MISSING | missing (MS) |
| folder "Ask about these meetings" | MeetingsScreen.swift:300 → ChatStore.swift:426 | sona_query_pack (folderId) | `work ask` asks every conversation; no folder filter | missing (MS; partial) |
| row menu "Add to folder…" / review menu "Add to folder…" | MeetingsScreen.swift:706; MeetingReviewView.swift:284; MeetingsStore.swift:873 | meeting_folders_for_session | MISSING | missing (MS) |
| filing sheet toggle a folder | MeetingsScreen.swift:546; MeetingsStore.swift:899 | meeting_folder_add_meeting / meeting_folder_remove_meeting | MISSING | missing (MS) |
| filing sheet "Create and add" / Done | MeetingsScreen.swift:560, :526 | meeting_folder_create + meeting_folder_add_meeting | MISSING | missing (MS) |

## 3. Delete, trash, export from list

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| row menu Delete and confirm | MeetingsScreen.swift:714, :618; MeetingsStore.swift:692 | meeting_delete | MISSING | missing (MS) |
| row menu "Export as Markdown" / "Export as JSON" | MeetingsScreen.swift:708-709; MeetingsStore.swift:914 | meeting_export | MISSING (`--meeting --format json` prints JSON; it does not write the export file) | missing (MS; partial) |
| row menu "Save the ledger as HTML" | MeetingsScreen.swift:711; MeetingsStore.swift:930 | produce_ledger_html | MISSING | missing (MS) |
| open trash | Shell.swift:276; MeetingsStore.swift:615, :628 | meeting_trash_list | MISSING | missing (MS) |
| trash Restore | MeetingsScreen.swift:865; MeetingsStore.swift:655 | meeting_trash_restore | MISSING | missing (MS) |
| trash "Delete forever" and confirm | MeetingsScreen.swift:867, :884; MeetingsStore.swift:675 | meeting_trash_delete_forever | MISSING | missing (MS) |
| trash Done | MeetingsScreen.swift:843 | none | n/a | ui-only-runtime (sheet) |

## 4. Meeting review: header and menu (MeetingReviewView.swift)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| edit title inline / menu "Rename" | MeetingReviewView.swift:239-244, :294; MeetingsStore.swift:1443 | meeting_title_set | MISSING | missing (MS) |
| "Try again" (load) | MeetingReviewView.swift:137 | meeting_get | `--meeting ID` | cli-exists |
| "Send to" menu → pick connection | MeetingReviewView.swift:278 → ConnectionsView.swift:194-207 | connections_snapshot | MISSING | missing (SET) |
| "Send to" review sheet preview | ConnectionsView.swift:212-258; ConnectionsStore.swift:196 | connections_preview_notes | MISSING | missing (MS + SET) |
| "Send notes" | ConnectionsView.swift:249; ConnectionsStore.swift:202 | connections_send_notes | MISSING (`work follow-up` drafts and sends nothing) | missing (MS + SET; token in keychain) |
| "Wrap up" | MeetingReviewView.swift:274; MeetingsStore.swift:2064 | meeting_wrap | `work follow-up` (who owes what, plus the follow-up text) | cli-exists |
| "Add a document…" | MeetingReviewView.swift:287; MeetingsStore.swift:1597 | doc_ingest | MISSING | missing (MS) |
| "Ask about this meeting" | MeetingReviewView.swift:290 → chat | sona_query_pack (session) | `work ask` (all conversations, not one meeting) | missing (MS; partial) |
| "Write the notes again" / "Try again" / "Write again" / "Write it again" | MeetingReviewView.swift:297, :389, :1388, :1679; MeetingNotesPane.swift:151; MeetingsStore.swift:1534 | meeting_artifacts_regenerate | MISSING | missing (MS; needs a model, so the runtime engine) |
| "Catch me up" | MeetingReviewView.swift:301; MeetingsStore.swift:2041 | meeting_catch_up | MISSING | missing (MS) |
| catch-up Close | MeetingReviewView.swift:2002 | none | n/a | ui-only-runtime (sheet) |
| "Copy notes" / "Copy notes as Markdown" | MeetingReviewView.swift:304, :306 (MeetingNotesCopy.swift) | none (pasteboard) | `--meeting ID` prints notes | ui-only-runtime (pasteboard) |
| menu Export MD / JSON | MeetingReviewView.swift:312-313 | meeting_export | MISSING | missing (MS; partial) |
| menu ledger HTML | MeetingReviewView.swift:316 | produce_ledger_html | MISSING | missing (MS) |
| menu Delete and confirm | MeetingReviewView.swift:324, :78 | meeting_delete | MISSING | missing (MS) |
| "Cancel the remote run" | MeetingReviewView.swift:371; MeetingsStore.swift:1562 | meeting_remote_cancel | MISSING | missing (RT: an in-flight remote job) |
| "Open Settings" | MeetingReviewView.swift:385 | none | n/a | ui-only-runtime (navigation) |
| analytics card load | MeetingsStore.swift:1698 | get_meeting_analytics | MISSING (`--stats` is global) | missing (MS) |
| talk-share speaker filter / stat jump and play | MeetingReviewView.swift:1086, :1103 | none (local filter, audio) | n/a | ui-only-runtime (view filter, audio playback) |
| keyword tracker "Show first" | MeetingReviewView.swift:1052 | none | n/a | ui-only-runtime (view sort) |
| people context card | MeetingsStore.swift:1881 | meeting_people_context | `--people NAME` / `work people` (per person, not per meeting) | missing (MS; partial) |

## 5. Transcript, speakers, speaker assignment

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| read transcript | MeetingsStore.swift:1061 | meeting_get | `--transcript ID`; `work meeting ID --transcript` | cli-exists |
| transcript search ⌘F, clear | MeetingReviewView.swift:574, :595; MeetingsStore.swift:1269 | meeting_search | MISSING (no in-meeting search; `--query` is corpus-wide) | missing (MS) |
| "Show everyone" | MeetingReviewView.swift:635 | none | n/a | ui-only-runtime (view filter) |
| speaker menu "Rename…", then save | MeetingReviewView.swift:797, :646; MeetingsStore.swift:1449 | meeting_speaker_rename | `work speaker-rename MEETING SPEAKER NAME` | cli-exists |
| speaker menu "Same person as X" | MeetingReviewView.swift:799; MeetingsStore.swift:1458 | meeting_speaker_merge | `work speaker-merge MEETING SPEAKER --into OTHER` (deletes the folded speaker's voice samples) | cli-exists |
| call names "Use <name>" suggestion | CallNamesView.swift:254 | meeting_speaker_rename | `work speaker-rename MEETING SPEAKER NAME` | cli-exists |
| call names "Dismiss" suggestion | CallNamesView.swift:252, :307 | meeting_call_name_dismiss | MISSING | missing (MS) |
| call names status / read | CallNamesView.swift:170, :293 | meeting_call_name_status | `--roster ID` | cli-exists |
| "Choose a call to read names…" / "Refresh calls" | CallNamesView.swift:126, :201 | meeting_call_name_targets | MISSING | ui-only-runtime (reads live call windows) |
| "Read names for this recording" / "Use clear matches automatically" / "Stop reading names" | CallNamesView.swift:137, :116, :123; :212 | meeting_call_names_set | MISSING | ui-only-runtime (binds to a live call window) |
| call prep | CallNamesView.swift:187 | meeting_call_prep | MISSING | ui-only-runtime (live call) |
| voice identify a speaker | PeopleStore.swift:873 | voice_identify_speaker | MISSING | missing (MS + voice profiles) |
| enroll a voice profile from a speaker | PeopleStore.swift:894 | voice_enroll_profile | MISSING | ui-only-deliberate (biometric voice profile, consent) |
| remove a voice profile | PeopleStore.swift:492 | voice_remove_profile | MISSING | ui-only-deliberate (biometric, privacy) |
| assign a speaker to a person (People screen) | PeopleStore.swift:767, :847 | meeting_speaker_rename | MISSING | missing (MS) |
| voice identity status | PeopleStore.swift:668 | voice_identity_status | MISSING | missing (MS; read) |
| tap a line → SegmentEditor Save / "Remove this line" | MeetingReviewView.swift:828, :908, :914; MeetingsStore.swift:1475 | meeting_segment_edit | `work line-edit MEETING LINE TEXT` (Save); removing a line stays in the app | cli-exists (Save) |
| play a turn | MeetingReviewView.swift:763 | conversation_audio_clip | n/a | ui-only-runtime (audio playback) |
| context menu "Listen with context" | MeetingReviewView.swift:847; ConversationAudioPlayback.swift:238, :264 | conversation_audio_retained / conversation_audio_clip | n/a | ui-only-runtime (audio playback) |
| context menu "Save passage…", then "Save passage" | MeetingReviewView.swift:850; ScratchpadView.swift:866; ScratchpadStore.swift:811 | scratchpad_capture_passage | MISSING | missing (NS + MS) |
| context menu "Copy source link" | MeetingReviewView.swift:851 | none (pasteboard) | n/a | ui-only-runtime (pasteboard) |
| recording tail "Cut the last N min…" and confirm "Cut" / "Put them back" | RecordingTailSection.swift:75, :37, :53; MeetingsStore.swift:1488 | meeting_segments_set_removed | MISSING | missing (MS) |
| recording tail read | RecordingTailSection.swift:101 | meeting_recording_tail | MISSING | missing (MS) |
| recording tail "Keep" | RecordingTailSection.swift:73 | none (view state) | n/a | ui-only-runtime (view state) |
| corrections "Teach for <name>" | TranscriptCorrectionsView.swift:517; :218 | transcript_correction_teach | `work correction-add MEETING LINE --heard W --fix W [--kind] [--context\|--around]` | cli-exists |
| corrections "Review & edit", then Save | TranscriptCorrectionsView.swift:404, :584; :238 | transcript_correction_update | `work correction-edit PERSON ID [--heard] [--fix] [--kind] [--context\|--around]` | cli-exists |
| corrections Pause/Resume | TranscriptCorrectionsView.swift:406 | transcript_correction_update | `work correction-edit PERSON ID --pause\|--resume` | cli-exists |
| corrections Remove and confirm | TranscriptCorrectionsView.swift:411, :351; :256 | transcript_correction_remove | `work correction-remove PERSON ID` | cli-exists |
| corrections list / review / Reload | TranscriptCorrectionsView.swift:321; :163, :170 | transcript_correction_list / transcript_correction_review | `work corrections PERSON`; lines with `--transcript ID` | cli-exists |
| corrections edit a line | TranscriptCorrectionsView.swift:189 | meeting_segment_edit | `work line-edit MEETING LINE TEXT` | cli-exists |
| corrections "Listen at source" / "Listen" | TranscriptCorrectionsView.swift:432, :488, :560 | conversation_audio_clip | n/a | ui-only-runtime (audio) |
| relisten "Accept" / "Skip" / "Accept all shown" | RelistenSheet.swift:378, :376, :348; :170 | transcript_relisten_decide | MISSING | missing (MS) |
| relisten offer read / "Review" / "Try again" / Done | RelistenSheet.swift:221, :229, :346; :196 | transcript_relisten_offer | MISSING | missing (MS) |

## 6. Notes on a meeting (generated notes, meeting notes, my notes)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| read generated notes | MeetingNotesPane.swift:13 | meeting_get | `--meeting ID` | cli-exists |
| action item done toggle | MeetingNotesPane.swift:250; MeetingsStore.swift:1668 | set_action_item_done | MISSING | missing (MS) |
| cited line → jump to moment | MeetingNotesPane.swift:212 | none | n/a | ui-only-runtime (navigation) |
| "Draft follow-up" | MeetingNotesPane.swift:135; MeetingsStore.swift:2084 | meeting_follow_up_draft | `work follow-up` | cli-exists |
| add a timestamped meeting note | MeetingReviewView.swift:1452, :1454; MeetingsStore.swift:1502 | meeting_note_create | MISSING | missing (MS) |
| edit a meeting note and save | MeetingReviewView.swift:1468, :1470; MeetingsStore.swift:1510 | meeting_note_update | MISSING | missing (MS) |
| delete a meeting note | MeetingReviewView.swift:1489; MeetingsStore.swift:1516 | meeting_note_delete | MISSING | missing (MS) |
| after-capture "Save takeaway" | ConversationAfterCaptureView.swift:78; :104 | meeting_note_create | MISSING | missing (MS) |
| after-capture "Review" | ConversationAfterCaptureView.swift:37 | none | n/a | ui-only-runtime (navigation) |
| my notes autosave / read | MeetingsStore.swift:1898, :1980 | get_meeting_user_notes / save_meeting_user_notes | MISSING (`--meeting` shows generated notes and ledger rows; my private notes are not read or written) | missing (MS) |
| "Rewrite with my notes" | MeetingReviewView.swift:1571; MeetingsStore.swift:2013 | reenhance_meeting_with_notes | MISSING | missing (MS; needs a model) |
| question "Forget" | MeetingReviewView.swift:1425; MeetingsStore.swift:1554 | meeting_question_forget | MISSING | missing (MS) |
| "Add a document…" list / marked copy "Save a marked copy…" | MeetingDocumentsCard.swift:127; :164, :179 | doc_marked_copy / doc_meeting_list | MISSING | missing (MS) |
| documents "Show in Finder" | MeetingDocumentsCard.swift:134 | none | n/a | ui-only-runtime (Finder) |

## 7. Ledger and loops

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| ledger rows read | MeetingsStore.swift:1745 | meeting_loops | `--meeting ID` (ledger rows); `--loops` | cli-exists |
| loop done toggle / Drop | MeetingReviewView.swift:1845, :1886; MeetingsStore.swift:1763 | meeting_loop_resolve | `--loop-resolve LOOP_ID` | cli-exists |
| loop Reopen | MeetingReviewView.swift:1890; MeetingsStore.swift:1769 | meeting_loop_reopen | MISSING | missing (MS) |
| loop owner menu | MeetingReviewView.swift:1936-1939; MeetingsStore.swift:1774 | meeting_loop_assign | MISSING | missing (MS) |
| loop "Open this person" / citation jump | MeetingReviewView.swift:1943, :1895 | none | n/a | ui-only-runtime (navigation) |
| "Play receipts" / Stop | MeetingReviewView.swift:1926, :1923 | conversation_reel_for_loop | n/a | ui-only-runtime (audio reel) |
| untracked actions read | MeetingsStore.swift:1803 | meeting_untracked_actions | MISSING | missing (MS) |
| untracked action Track | UntrackedActionsSection.swift; MeetingsStore.swift:1828 | planning_track_action | `work create` makes a task, but not linked to the action | missing (HDB planning) |
| untracked action Dismiss / Restore | MeetingsStore.swift:1845, :1861 | planning_dismiss_action / planning_restore_action | MISSING | missing (HDB planning) |
| live card owner menu "Me" / person / "No owner" | LiveCard.swift:322, :330; :169 | meeting_live_card_owner | MISSING | missing (MS) |
| live card Accept / Decline | LiveCard.swift:484-485 | tracker accept/decline | `work accept` / `work decline` | cli-exists |

## 8. Wrap-up and follow-up

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| wrap sheet load | MeetingWrapSheet.swift:160 | meeting_wrap | `work follow-up` | cli-exists |
| "Write the follow-up" | MeetingWrapSheet.swift:145 | meeting_follow_up_draft | `work follow-up` | cli-exists |
| "Copy for chat" | MeetingWrapSheet.swift:73 | none (pasteboard) | `work follow-up` prints the text | ui-only-runtime (pasteboard) |
| "Open in Mail" | MeetingWrapSheet.swift:76; MeetingsStore.swift:2116, :2127 | meeting_follow_up_compose / meeting_follow_up_mail, plus meeting_prep_preferences_get | MISSING | ui-only-runtime (opens Mail.app compose) |
| wrap "Play said" | MeetingWrapSheet.swift:97, :121 | conversation_audio_clip | n/a | ui-only-runtime (audio) |
| wrap Close | MeetingWrapSheet.swift:54 | none | n/a | ui-only-runtime (sheet) |
| live wrap card "Copy follow-up" | MeetingLiveView.swift:1514; AppModel.swift:1194 | meeting_follow_up_draft | `work follow-up` | cli-exists |
| live wrap card Done | MeetingLiveView.swift:1520; MeetingLiveStore.swift:718 | meeting_ritual_respond | MISSING | ui-only-runtime (ritual panel of a just-ended live call) |
| live card deliver route ⌘↩ | LiveCard.swift:512; :219 | connections_send_notes route | MISSING | missing (MS + SET) |

## 9. Connections (send to services)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| list connections / Refresh / Try again | ConnectionsView.swift:17, :103; ConnectionsStore.swift:148 | connections_snapshot | MISSING | missing (SET) |
| "Connect a service" / Edit, then "Save connection" | ConnectionsView.swift:38, :31, :152; ConnectionsStore.swift:154 | connections_save | MISSING | ui-only-deliberate (API token or signing secret into the keychain) |
| "Allow Sona to send to this destination" toggle | ConnectionsView.swift:140 | connections_save (enabled) | MISSING | ui-only-deliberate (consent to send off the Mac) |
| Test | ConnectionsView.swift:32; ConnectionsStore.swift:168 | connections_test | MISSING | missing (SET; network send) |
| Disconnect and confirm | ConnectionsView.swift:33, :111; ConnectionsStore.swift:162 | connections_disconnect | MISSING | ui-only-deliberate (removes the keychain secret) |
| "Add an automatic send", consent toggle, "Start sending" | ConnectionsView.swift:58, :181, :187; ConnectionsStore.swift:182 | connections_rule_save | MISSING | ui-only-deliberate (standing consent to send future notes) |
| "Stop sending" | ConnectionsView.swift:53; ConnectionsStore.swift:191 | connections_rule_delete | MISSING | ui-only-deliberate (consent revoke; kept with its grant) |
| "Email drafts" / "Calendar events" toggles | ConnectionsView.swift:69, :79; ConnectionsStore.swift:174 | connections_preferences_save | MISSING | ui-only-deliberate (enables chat actions that reach Mail and Calendar) |
| recent send Undo | ConnectionsView.swift:98; ConnectionsStore.swift:213 | connections_undo_send | MISSING | missing (SET + network) |
| "Connections settings" | ConnectionsView.swift:204 | none | n/a | ui-only-runtime (navigation) |

## 10. Share (cloud links)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| meeting "Create browser share" | CloudSyncView.swift:366; CloudSyncStore.swift:456-460 | cloud_browser_share_create | MISSING | missing (CS; publishes a link, so needs a careful guard) |
| meeting share create | CloudSyncStore.swift:398 | cloud_share_create | MISSING | missing (CS) |
| meeting "Export .sona file" | CloudSyncView.swift:363 (store.exportBundle) | cloud sync export (CloudSyncStore) | MISSING (`--export-meetings` writes a different format) | missing (CS + MS) |
| meeting share list / "Copy link" | CloudSyncStore.swift:202; CloudSyncView.swift:411 | cloud_share_list | MISSING | missing (CS); copy is pasteboard |
| meeting share Revoke | CloudSyncView.swift:443; CloudSyncStore.swift:475 | cloud_share_revoke | MISSING | missing (CS) |
| import a shared file | CloudSyncStore.swift:450 | cloud_share_import_file | MISSING | missing (CS + MS) |
| note share button | ScratchpadView.swift:439 → Shell.swift:135-145 | none (opens sheet) | n/a | ui-only-runtime (sheet) |
| note share list / Refresh | CloudNoteShareView.swift:85; :143 | cloud_note_share_list | MISSING | missing (CS + NS) |
| note share create / browser link | CloudNoteShareView.swift:156, :172 | cloud_note_share_create / cloud_browser_share_link | MISSING | missing (CS + NS) |
| note share Revoke and confirm | CloudNoteShareView.swift:109, :133; :186 | cloud_share_revoke | MISSING | missing (CS) |

## 11. Import audio and documents (ImportView.swift, ImportStore.swift)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| "Choose files" / drop files | ImportView.swift:34, :49 | none (row list) | n/a | ui-only-runtime (file picker, drag and drop) |
| run import | ImportStore.swift:104-130 | import_chosen_media_file (native_bridge/mod.rs:351) | MISSING (`-f` makes no history entry; positional AUDIO only forwards to the GUI) | missing (HDB or MS, depending on how the file routes) |
| Retry / Remove a row | ImportView.swift:167-170 | none | n/a | ui-only-runtime (view queue) |
| import job list | ImportStore.swift:49 | list_audio_import_jobs | MISSING | missing (RT: the job queue lives in the running app) |
| Cancel a job | ImportView.swift:200; ImportStore.swift:151-155 | cancel_audio_import | MISSING | ui-only-runtime (cancels an in-process job) |
| Open a routed result / Open job result | ImportView.swift:125, :202; ImportStore.swift:169 | sona_open_link | n/a | ui-only-runtime (navigation) |
| routed Dismiss | ImportView.swift:129 | none | n/a | ui-only-runtime |
| Library import icon | LibraryScreen.swift:106 → Shell.swift:118 | none (opens Settings > Importing) | n/a | ui-only-runtime (navigation) |
| "Add document" | ImportView.swift:221; ImportStore.swift:249, :267 | doc_ingest | MISSING | missing (MS) |
| documents list | ImportStore.swift:236 | doc_list | MISSING | missing (MS) |
| document Delete | ImportView.swift:274; ImportStore.swift:285 | doc_delete | MISSING | missing (MS) |

## 12. Live recording and start (MeetingLiveView.swift, MeetingStartView.swift, MeetingLiveStore.swift)

All of these drive the live recorder, the audio device or the in-call overlay. None can run headless. `--export-meetings` refuses while a recording is running.

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| Record / Record… / Start transcribing | MeetingStartView.swift:101, :242, :465, :473; MeetingLiveStore.swift:916 | meeting_start | MISSING | ui-only-runtime (microphone and system audio capture) |
| start screen Refresh | MeetingStartView.swift:98; MeetingLiveStore.swift:880, :959, :996 | meeting_preflight_create / meeting_preflight_refresh / meeting_preflight_cancel | MISSING | ui-only-runtime (checks the audio devices) |
| suggestion Skip / Open brief | MeetingStartView.swift:236, :239; :463 | meeting_suggestions_list | `--upcoming` (read only) | ui-only-runtime (Skip); the list read is cli-exists via `--upcoming` |
| detection prompt Dismiss | MeetingStartView.swift:468; MeetingLiveStore.swift:675, :689 | detection_prompt_respond / detection_keep_recording | n/a | ui-only-runtime (live detection prompt) |
| recovery Finish | MeetingStartView.swift:564; MeetingLiveStore.swift:1520 | meeting_recovery_finalize | MISSING | missing (MS; interrupted recordings could be finalized headless) |
| recovery Discard and confirm | MeetingStartView.swift:561, :578 | meeting_discard | MISSING | missing (MS) |
| recovery list | MeetingLiveStore.swift:487 | meeting_recovery_list | MISSING | missing (MS) |
| "Turn on Use my calendar in Settings" | MeetingStartView.swift:618 | none | n/a | ui-only-deliberate (calendar permission) |
| Pause / Resume | MeetingLiveView.swift:194, :191; MeetingLiveStore.swift:1156, :1160 | meeting_pause / meeting_resume | n/a | ui-only-runtime (live recorder) |
| Stop | MeetingLiveView.swift:164; MeetingLiveStore.swift:1173 | meeting_stop | n/a | ui-only-runtime (live recorder) |
| "Read meeting" | MeetingLiveView.swift:168 | none (navigation) | n/a | ui-only-runtime |
| Discard and "Stop and discard" | MeetingLiveView.swift:201, :100; MeetingLiveStore.swift:1194 | meeting_discard | n/a | ui-only-runtime (live recorder) |
| Bookmark ⇧⌘B / typed bookmarks ⌃⌥key | ConversationCaptureControls.swift:14, :48; MeetingLiveView.swift:156, :162; MeetingLiveStore.swift:1466, :1448 | conversation_bookmark_create / conversation_bookmarks | `--bookmarks` reads them | ui-only-runtime (needs the live recording clock); read is cli-exists |
| "Add a note" (live) | MeetingLiveView.swift:197, :567; MeetingLiveStore.swift:1421 | meeting_note_create | MISSING | ui-only-runtime (stamps the live offset) |
| live notes "Keep what I wrote" / autosave | MeetingLiveView.swift:374; MeetingLiveStore.swift:1284 | save_meeting_user_notes | MISSING | missing (MS) |
| live notes "Copy my notes" | MeetingLiveView.swift:377 | none | n/a | ui-only-runtime (pasteboard) |
| live help kinds / "Ask" / submit / "Try again" / Dismiss | MeetingLiveView.swift:393, :403, :405, :439, :427; MeetingLiveStore.swift:1377 | meeting_live_help | MISSING | ui-only-runtime (reads the live transcript buffer) |
| live recall | MeetingLiveStore.swift:1138 | meeting_live_recall | MISSING | ui-only-runtime (live) |
| live transcript | MeetingLiveStore.swift:626 | meeting_live_transcript | MISSING | ui-only-runtime (live) |
| call silence | MeetingLiveStore.swift:1718 | meeting_call_silence | n/a | ui-only-runtime (audio level) |
| consent panel start / forget series / fit disclosure | MeetingLiveStore.swift:807, :829, :763 | meeting_consent_panel_start / meeting_consent_panel_forget_series / meeting_consent_panel_fit_disclosure | n/a | ui-only-deliberate (recording consent) |
| ritual panel ack | MeetingLiveStore.swift:703 | meeting_ritual_panel_ack | n/a | ui-only-runtime (overlay) |
| live card question / Enter | LiveCard.swift:417; :146 | meeting_live_card | MISSING | ui-only-runtime (live card) |
| next-meeting text engine | MeetingLiveStore.swift:502 | meeting_text_engine_for_next_meeting | MISSING | missing (SET) |
| quit stops a recording | SonaApp.swift:160 | meeting_stop | n/a | ui-only-runtime |

## 13. Snapshots and open windows

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| take a snapshot (live menu item) | MeetingLiveView.swift:199; MeetingSnapshots.swift:194 | meeting_snapshot_take | n/a | ui-only-runtime (screen capture) |
| "Delete snapshot" (context menu, viewer) | MeetingSnapshots.swift:405, :460; :207 | meeting_snapshot_delete | MISSING | missing (MS) |
| automatic snapshots setting | MeetingSnapshots.swift:224 | meeting_snapshot_automatic_set | MISSING | ui-only-deliberate (screen capture consent) |
| snapshot text setting | MeetingSnapshots.swift:260 | meeting_snapshot_text_set | MISSING | ui-only-deliberate (on-screen text capture, privacy) |
| "Open Screen Recording settings" | MeetingSnapshots.swift:307, :520 | none | n/a | ui-only-deliberate (permission prompt) |
| snapshot list, status, open image, "Try again", Done | MeetingSnapshots.swift:160-161, :387, :421, :480, :355, :454, :447 | meeting_snapshot_list / meeting_snapshot_status / meeting_snapshot_image | MISSING | missing (MS; image viewing is a window) |
| open windows "Attach" / "Attach all" | MeetingOpenWindows.swift:80, :88; :125 | meeting_open_window_attach | n/a | ui-only-runtime (live window list) |
| open windows Dismiss | MeetingOpenWindows.swift:86; :159 | meeting_open_windows_dismiss | n/a | ui-only-runtime |
| attached link Remove | MeetingOpenWindows.swift:110; :144 | meeting_open_window_remove | MISSING | missing (MS) |
| attached link Open | MeetingOpenWindows.swift:105 | none (NSWorkspace) | n/a | ui-only-runtime |
| open windows list | MeetingOpenWindows.swift:171 | meeting_open_windows | MISSING | ui-only-runtime (live window list) |

## 14. Context, place, plans

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| context read / "Try again" | ConversationContextView.swift:158; :188 | conversation_context | `--place ID` (partial) | missing (MS; partial) |
| "Context…", then Save | ConversationContextView.swift:148, :321; :375 | conversation_context_update | MISSING | missing (MS) |
| "Search Maps" / submit | ConversationContextView.swift:259-260 | MapKit search (no bridge) | n/a | ui-only-runtime (MapKit) |
| hotel "Use this place…" / "Not this" | ConversationContextView.swift:212, :210; :385 | conversation_place_from_mail | MISSING | ui-only-runtime (reads Mail) |
| place card read / Retry | ConversationPlaceCard.swift:157; :166, :235 | conversation_place_review / conversation_bookmarks | `--place ID`, `--bookmarks` | cli-exists |
| take a place reading | PlaceHere.swift:40 | conversation_place_reading_take | n/a | ui-only-deliberate (location permission) |
| context suggestion "Use" / "Not this" / "Change", then "Use" | ContextSuggestionsView.swift:34, :28, :31, :135; :59, :73 | context_suggestion_decide / context_suggestions | MISSING | missing (MS) |
| heard plans read | HeardPlansCard.swift:104 | meeting_heard_plans | `--plans` | cli-exists |
| heard plan Edit, then Save | HeardPlansCard.swift:165, :257; :133 | meeting_heard_plan_edit | MISSING | missing (MS) |
| heard plan "Not now" | HeardPlansCard.swift:167 | meeting_heard_plan_not_now | MISSING | missing (MS) |
| heard plan "Add to calendar" / Undo | HeardPlansCard.swift:188, :171 | meeting_heard_plan_add_to_calendar / meeting_heard_plan_undo_calendar | MISSING | ui-only-deliberate (writes to the user's calendar through the EventKit permission) |
| heard plan "Open the recording" | HeardPlansCard.swift:175 | none | n/a | ui-only-runtime (navigation) |
| phone thought source / photo | PhoneThoughtSourceSheet.swift:117, :146 | phone_thought_source_get / phone_thought_photo_get | MISSING | missing (NS) |

## 15. Digest

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| "Evening digest" toggle | MeetingSettingsView.swift:759-765; MeetingSettingsStore.swift:589-592 | change_meeting_digest_enabled_setting | MISSING | missing (SET) |
| "Digest time" field | MeetingSettingsView.swift:775-792; MeetingSettingsStore.swift:599-604 | change_meeting_digest_minute_of_day_setting | MISSING | missing (SET) |
| series "Include in the evening digest" | MeetingSettingsView.swift:1166-1174; OverviewView.swift:776-781; CaptureScreen.swift:214; MeetingSettingsStore.swift:1031-1033 | meeting_series_digest_set | MISSING | missing (MS series settings) |
| Home evening read | EveningStore.swift:83 | home_evening | MISSING | missing (MS + HDB) |
| "Sum up today" / "Rewrite" | EveningView.swift:175, :163; EveningStore.swift:56 | home_evening_write | MISSING | missing (MS + HDB; needs a model) |
| filing "File" / "Not this" | EveningView.swift:211, :213; EveningStore.swift:67 | filing_decide | MISSING | missing (MS) |
| voice-at / said / planned-in jumps | EveningView.swift:254, :267, :285 | none | n/a | ui-only-runtime (navigation, audio) |

## 16. Notes (scratchpad: ScratchpadView.swift, ScratchpadStore.swift)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| notes list / "Try again" | ScratchpadView.swift:91; ScratchpadStore.swift:359 | scratchpad_list | `--notes` | cli-exists |
| "Search notes" field | ScratchpadView.swift:65 | scratchpad_list (query) | `--query --scope notes` | cli-exists |
| open a note | ScratchpadStore.swift:686 | scratchpad_get | `--note NOTE_ID` | cli-exists |
| "+" / "New note" | ScratchpadView.swift:67, :106; ScratchpadStore.swift:516 | scratchpad_create | MISSING | missing (NS) |
| edit body (autosave) | ScratchpadStore.swift:514 | scratchpad_update_body | MISSING | missing (NS) |
| Pin / Unpin | ScratchpadView.swift:424; ScratchpadStore.swift:572 | scratchpad_set_pinned | MISSING | missing (NS) |
| Delete and confirm | ScratchpadView.swift:446, :261; ScratchpadStore.swift:590 | scratchpad_delete | MISSING | missing (NS) |
| version history | ScratchpadView.swift:432; ScratchpadStore.swift:617 | scratchpad_versions | `--note-versions NOTE_ID`; `--note-diff` | cli-exists |
| version Restore / "Return to current" | ScratchpadView.swift:572, :923; ScratchpadStore.swift:655 | scratchpad_restore_version | MISSING | missing (NS) |
| sources (link icon), memory, evidence | ScratchpadView.swift:226; ScratchpadStore.swift:704, :717 | scratchpad_memory / scratchpad_source_evidence | `--note NOTE_ID` (sources, related); `--note-context TEXT` | cli-exists |
| link a source (search, then pick) | ScratchpadStore.swift:726, :736 | sona_query_search + scratchpad_link | MISSING (search exists via `--query`; the link does not) | missing (NS) |
| unlink a source (x) | ScratchpadView.swift:354; ScratchpadStore.swift:750 | scratchpad_unlink | MISSING | missing (NS) |
| "Open original" / "Open moment" / "Done" | ScratchpadView.swift:277, :313, :333, :281 | none / phone_thought_source_get | n/a | ui-only-runtime (navigation) |
| "Review connections", then "Write note" / "Review with AI" | ScratchpadView.swift:63, :224, :670, :672; ScratchpadStore.swift:778, :784, :789 | scratchpad_prepare_synthesis / scratchpad_review_sources / scratchpad_review_candidates | MISSING | missing (NS + MS; AI review needs a model) |
| synthesis "Accept as new note" / "Reject" / Differences | ScratchpadView.swift:780, :779, :802; ScratchpadStore.swift:797 | scratchpad_accept_synthesis | MISSING | missing (NS) |
| synthesize | dispatch.rs | scratchpad_synthesize | MISSING | missing (NS) |
| "Save passage" (from transcript) and "Remove person" | ScratchpadView.swift:866, :887; ScratchpadStore.swift:811 | scratchpad_capture_passage | MISSING | missing (NS + MS) |
| "Subjects" list | ScratchpadView.swift:62, :950; ScratchpadStore.swift:824 | scratchpad_subjects / scratchpad_subject | MISSING | missing (NS) |
| subject "Create" | ScratchpadView.swift:995 | scratchpad_subject_create | MISSING | missing (NS) |
| subject "Add open note" / "Use open note as current" / "Set current" / "Remove" | ScratchpadView.swift:963-964, :984, :986 | scratchpad_subject_note | MISSING | missing (NS) |
| "Delete subject" and confirm | ScratchpadView.swift:955, :1039 | scratchpad_subject_delete | MISSING | missing (NS) |
| recent notes (Overview) | OverviewStore.swift:306 | scratchpad_recent | `--notes` | cli-exists |

## 17. Search

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| command palette corpus search, typing, load more | Panels.swift:1546-1624 (schedule, loadMore, ask) | sona_query_search | `--query TEXT [--scope]` (first page only; no cursor flag) | cli-exists |
| palette "said" lines | Panels.swift:1537 | meeting_said_search | MISSING | missing (MS) |
| palette on-screen text matches | Panels.swift:1538 | meeting_snapshot_text_search | MISSING | missing (MS) |
| palette task command / answer | Panels.swift (work.command, work.ask) | tracker | `work ask`, `work create/update` | cli-exists |
| palette row Play / Stop | Panels.swift (returnVerb, PaletteRowView) | conversation_audio_clip | n/a | ui-only-runtime (audio) |
| QueryStore search, events, pack, open | ImportStore.swift:341-429 | sona_query_search / sona_query_events / sona_query_pack / sona_open_link | `--query`, `--events`, `work ask` | cli-exists. QueryStore is built at AppModel.swift:393, but no view call was found. |
| meeting list title search | see section 1 | meeting_list | MISSING | missing (MS) |
| in-meeting transcript search | see section 5 | meeting_search | MISSING | missing (MS) |

## 18. Recordings history (dictations: LibraryScreen.swift, LibraryStore.swift)

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| history list, "Load more", "Try again" | LibraryScreen.swift:530, :526, :378; LibraryStore.swift:210 | get_history_entries | `work dictations [--search T] [--limit N] [--before ID]` | cli-exists |
| history search field | LibraryScreen.swift (search field near :105); LibraryStore.swift:212 | search_history_entries | `--query TEXT --scope dictations` | cli-exists |
| stats / stats "Try again" | LibraryScreen.swift:120; LibraryStore.swift:344 | get_history_stats | `--stats` (partial) | cli-exists |
| trend range picker | LibraryScreen.swift:180; LibraryStore.swift:358 | get_history_trend | MISSING | missing (HDB) |
| run receipts | LibraryStore.swift:371 | get_history_run_receipts | MISSING | missing (HDB) |
| expand a row | LibraryScreen.swift:574, :479 | none | n/a | ui-only-runtime |
| "Show raw text" toggle | LibraryScreen.swift:144 | none (view mode) | n/a | ui-only-runtime |
| "Open recordings folder" | LibraryScreen.swift:149; LibraryStore.swift:500 | open_recordings_folder | n/a | ui-only-runtime (Finder) |
| play / pause | LibraryScreen.swift:756; LibraryStore.swift:655 | read_history_audio_chunk | n/a | ui-only-runtime (audio playback) |
| "Transcribe" / "Transcribe again" | LibraryScreen.swift:320, :700; LibraryStore.swift:477 | retry_history_entry_transcription | MISSING (`-f` transcribes a file but writes no entry) | window-bound (reruns the app's speech model) |
| "Save entry" / "Remove from saved" | LibraryScreen.swift:708; LibraryStore.swift:435 | toggle_history_entry_saved | `work dictation-save ID [--unsave]` | cli-exists |
| "Process again", pick mode, "Process" | LibraryScreen.swift:712, :1180, :1202; LibraryStore.swift:495 | reprocess_history_entry | MISSING | window-bound (reruns the app's cleanup model) |
| Delete and confirm | LibraryScreen.swift:325, :715, :347; LibraryStore.swift:461 | delete_history_entry | `work dictation-delete ID` | cli-exists |
| "Add correction", then "Save correction" | LibraryScreen.swift:706, :1143; Shell.swift:119; VocabularyStore.swift:391 | add_vocabulary_correction | MISSING | missing (SET vocabulary) |
| storage status | LibraryStore.swift:680 | history_storage_status | MISSING | missing (HDB) |
| history limit field | LibraryScreen.swift:1064; LibraryStore.swift:697 | update_history_limit | MISSING | missing (SET) |
| recordings retention choice | LibraryScreen.swift:1066; LibraryStore.swift:714 | update_recording_retention_period | MISSING | ui-only-deliberate (retention and deletion of audio, privacy) |

## 19. Meeting settings related to meetings

| UI action | Swift trigger | bridge method | CLI | class |
|---|---|---|---|---|
| meeting retention / transcript retention / audio retention | MeetingSettingsStore.swift:778, :793, :816, :831, :848, :863 | meeting_retention_get/set and the transcript and audio variants | MISSING | ui-only-deliberate (privacy retention) |
| keyword trackers list / save | MeetingSettingsStore.swift:879, :913 | list_keyword_trackers / save_keyword_trackers | MISSING | missing (SET) |
| automation roster / per-series automation / runs | MeetingSettingsStore.swift:933, :968, :992 | meeting_automation_roster / meeting_series_automation_set / meeting_automation_runs | MISSING | missing (MS series settings) |
| series template | MeetingSettingsStore.swift:1023 | meeting_series_template_set | MISSING | missing (MS series settings) |
| series always record | MeetingSettingsStore.swift:1044 | meeting_series_always_record_set | MISSING | ui-only-deliberate (standing recording consent) |
| series remote roster | MeetingSettingsStore.swift:1061 | meeting_series_remote_roster | MISSING | missing (MS) |
| custom templates list / save / delete | MeetingTemplatesStore.swift:39, :109, :130 | meeting_custom_templates_list / meeting_custom_template_save / meeting_custom_template_delete | MISSING | missing (SET) |
| learn / forget my voice | MeetingPrepStore.swift:309, :329 | voice_user_learn / voice_user_forget | n/a | ui-only-deliberate (biometric consent) |
| web research key set / remove | MeetingPrepStore | meeting_web_research_key_set / meeting_web_research_key_remove | n/a | ui-only-deliberate (keychain) |

## Biggest gaps

The CLI reads meetings well: list, summary, notes, ledger, transcript, roster, plans, bookmarks, place and follow-up. It reads notes well too: list, read, versions, diff and context.

The only meeting write the CLI has is closing a loop.

Missing write verbs, with their data location:
- Meeting writes (MS): rename, line cuts, meeting notes and my notes, folders, delete and trash, export to md/json/csv/ledger html, rewrite notes, loop reopen and owner, action item done, relisten, heard plan edit, context, import of a recording or transcript.
- Scratchpad writes (NS): create, edit, pin, delete, restore, link, synthesis, subjects, save passage.
- Dictation history (HDB): transcribe again and process again stay in the app, since they rerun its models.
- Connections: send notes and undo send. The security parts stay in the UI.
- Cloud share links (CS).
