# Sona UI to CLI inventory: Talk to Sona, Home, Settings (non-consent); Notes and Search pointer

> Summary table with an MCP column and current statuses: [README.md](README.md). This file keeps the per-button Swift evidence. The view display rows below say `work view-save` covers empty groups and shown properties; it has no flag for either, so README.md lists them as missing.

Read-only; no builds. Swift paths relative to macos/Sona/. Bridge names are `core.request` strings, checked against dispatch.rs (/private/var/tmp/bridge-methods.txt). CLI = `sona-core` flags in src-tauri/src/cli.rs and `sona-core work <verb>` in src-tauri/src/query/external/tracker.rs. There is no CLI for chat turns, chat history, model choice, or any settings read/write; `--agent-panel-public-identity` is the only chat-related flag. Classes: cli-ok, missing, ui-only (consent / privacy / security / permission / token = deliberate; runtime = needs a window, pasteboard, Finder, live keyboard or audio).

Summary
1. 318 actions listed: 18 cli-ok, 189 missing, 51 deliberate ui-only, 60 ui-only for runtime reasons (1 pointer row).
2. Talk to Sona has no CLI at all: send/stop/retry a turn, chat history, model and effort, apply/undo/dismiss proposals and actions; `work ask` answers from local lines only.
3. Plan mode: the AI draft (planning_prepare/revise) is missing; the accepted result can be rebuilt with `work project-save`, `milestone-save`, `create`.
4. Home dictation start/stop/cancel already work via `sona --toggle-transcription` / `--cancel`; tasks.md now says so too. Most Home cards are in tasks.md.
5. Settings: no settings get/set verb exists, so every pane (essentials, dictation, modes, models, providers, vocabulary, prompts, workflows, prep, sync, about, debug) is missing apart from `--list-models`; keys, pairing, bridge permissions and sharing stay deliberate ui-only.

## 1. Talk to Sona: Ask and Configure modes (ChatView.swift, ChatStore.swift, ChatThread.swift, ChatWork.swift, ChatModelPicker.swift, ChatActionReview.swift)

| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| open chat sheet (load status, history, settings, models) | ChatStore.swift:106, :237, :254, :263, :291 | agent_panel_status, agent_chat_history_list, get_app_settings, agent_panel_models | `--agent-panel-public-identity` only; no status/history/model list | missing |
| mode picker Ask / Configure / Plan | ChatView.swift:85, :309, :326 | none (sets workspace sent with each turn) | MISSING | missing |
| type + Return / Send (Ask: builds quoted evidence, then sends turn) | ChatView.swift:736, :763; ChatStore.swift:345, :378, :423 | sona_query_pack + agent_panel_send_turn | `work ask "Q"` answers from local corpus lines, not the remote agent turn; `--query` | missing (remote agent turn) |
| Send in Configure (settings change by talking) | ChatView.swift:763; ChatStore.swift:378 (no pack) | agent_panel_send_turn (workspace sona_config) | MISSING | missing |
| suggestion chip pressed with empty field | ChatStore.swift:352 | sona_query_pack + agent_panel_send_turn | as Send | missing |
| Stop (running turn) / Cancel in work log | ChatView.swift:759; ChatWork.swift:83; ChatStore.swift:446, :451 | agent_panel_cancel_turn | MISSING | missing |
| Retry failed question | ChatView.swift:547; ChatWork.swift:39; ChatStore.swift:372 | agent_panel_send_turn | MISSING | missing |
| notice Retry (re-read panel) | ChatView.swift:243; ChatStore.swift:228 | agent_panel_test_connection + agent_panel_status | MISSING | missing |
| notice "Settings" / "Open connection settings" | ChatView.swift:246; ChatModelPicker.swift:157, :172 | none (navigation to Settings > Agents) | n/a | ui-only (runtime: navigation) |
| "Allow" meeting intelligence (sends quotes to server) | ChatView.swift:260; ChatStore.swift:805 | change_meeting_remote_intelligence_enabled_setting | none | ui-only (consent) |
| folder scope "Remove" | ChatView.swift:299; ChatStore.swift:750 | none (clears folder_id for next turn) | MISSING (`work ask` has no folder filter) | missing |
| scope chat to a folder (from Meetings "Ask about these meetings") | Shell.swift:353; ChatStore.swift:731 | agent_chat_new + sona_query_pack (folderId) | MISSING | missing |
| "Talk to Sona" about project (seeds draft) | AppModel.swift:662; ProjectsView.swift:417; ChatStore.swift:124 | none until Send | MISSING | missing |
| New chat | ChatView.swift:100; ChatStore.swift:755 | agent_chat_new | MISSING | missing |
| Recent chats menu: open a past chat | ChatView.swift:131; ChatStore.swift:770 | agent_chat_open | MISSING (no history list/read) | missing |
| Close chat | ChatView.swift:111 | none | n/a | ui-only (runtime: window) |
| model picker: search + Return, pick model | ChatModelPicker.swift:95, :202; ChatStore.swift:311, :326 | agent_panel_select_model | MISSING (`--list-models`/`--model` are local transcription models, not chat models) | missing |
| "Use server default" | ChatModelPicker.swift:241; ChatStore.swift:311 | agent_panel_select_model (null) | MISSING | missing |
| thinking effort picker | ChatModelPicker.swift:253; ChatStore.swift:333 | agent_panel_select_model | MISSING | missing |
| Attach "Use a window…" / "Choose screenshot…" | ChatView.swift:773, :774; ChatStore.swift:466; ChatScreenshot.swift:115 | none until Send (screen-sharing picker) | n/a | ui-only (permission: screen capture) |
| screenshot "Remove" | ChatView.swift:813; ChatStore.swift:498 | none | n/a | ui-only (runtime) |
| voice conversation start / end | ChatView.swift:750; ChatStore.swift:506, :521 | chat_voice_start, chat_voice_stop | MISSING | ui-only (permission: microphone) |
| spoken answer playback | ChatStore.swift:614, :630 | chat_voice_speak | n/a | ui-only (runtime: audio) |
| voice turn auto-send after speech | ChatStore.swift:580, :599 | agent_panel_send_turn / agent_panel_cancel_turn | MISSING | ui-only (permission: microphone) |
| proposal "Apply" (Configure change) | ChatView.swift:598; ChatStore.swift:651 | agent_panel_apply_change | MISSING | missing |
| proposal "Undo" | ChatView.swift:604; ChatStore.swift:668 | agent_panel_undo_change | MISSING | missing |
| action "Apply" (resolve/assign loop, rename speaker, vocabulary term, series template) | ChatView.swift:652; ChatStore.swift:684; ChatTypes.swift:354-367 | agent_panel_apply_action | partial: `--loop-resolve`, `work person-save`; rename speaker / vocabulary / series template MISSING | missing (partial) |
| action "Review" → edit + approve (email draft, Slack post, calendar event, send notes) | ChatView.swift:652; ChatActionReview.swift:56, :132-143; ChatStore.swift:710 | connections_edit_chat_action + agent_panel_apply_action | none | ui-only (consent: sends to outside services in owner's name) |
| review sheet "Try again" / "Cancel" | ChatActionReview.swift:51, :54, :161 | connections_list (via ConnectionsStore.load) | n/a | ui-only (runtime) |
| action "Dismiss" / "Undo" (dismissed) | ChatView.swift:658, :666; ChatStore.swift:688 | agent_panel_dismiss_action | MISSING | missing |
| click `sona://` citation link | ChatThread.swift:44; ChatStore.swift:793 | sona_open_link | `--meeting ID --transcript`, `--note ID` read the target | cli-ok (read) / ui-only (navigation) |
| work-log disclosure (show steps) | ChatWork.swift:97 | none (polls status) | n/a | ui-only (runtime) |
| status polling of a running turn | ChatStore.swift:694-701 | agent_panel_status | MISSING | missing |
| people names for action cards | ChatStore.swift:888 | people_list | `work people`, `--people` | cli-ok |
| "Replies waiting for you" row | ChatView.swift:267; Shell.swift:57 | none (opens Settings > Agents requests) | n/a | ui-only (runtime: navigation) |

## 2. Talk to Sona: Plan mode (PlannerAgentView.swift, PlannerAgentStore.swift)

| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| "Plan with Sona" opens Plan mode | AppModel.swift:667; ProjectsView.swift:161, :168; ChatStore.swift:117 | none | n/a | ui-only (runtime: navigation) |
| goal field + Send (⌘Return): draft a project | PlannerAgentView.swift:443; PlannerAgentStore.swift:242, :268 | planning_prepare | MISSING (AI draft); manual: `work project-save`, `milestone-save`, `create` | missing |
| reply to planner (revise draft) | PlannerAgentView.swift:443; PlannerAgentStore.swift:265 | planning_revise | MISSING | missing |
| "Use revision" / "Keep previous" | PlannerAgentView.swift:395, :393; PlannerAgentStore.swift:295, :308 | none (local draft) | n/a | ui-only (runtime: draft state) |
| edit title / description / milestones / tasks, add/remove rows, unlink commitment, remove citation, due-date toggle | PlannerAgentView.swift:185-300, :361, :505, :510, :595; PlannerAgentStore.swift:213, :225, :233 | none until accept | n/a (CLI equivalents act on saved projects) | ui-only (runtime: draft state) |
| "Choose…" sources: list meetings/notes/loops, search, Load more, Use selected | PlannerAgentView.swift:161, :650-684, :724; ProjectsStore.swift:621-624 | meeting_list, scratchpad_list, meeting_loops | `--meetings`, `--notes`, `--loops` (read) | cli-ok (read) |
| remove a source | PlannerAgentView.swift:173; PlannerAgentStore.swift:197 | none | n/a | ui-only (runtime) |
| source "Open" | PlannerAgentView.swift:324 | none (navigation) | n/a | ui-only (runtime: navigation) |
| speak message (mic) | PlannerAgentView.swift:430; PlannerAgentStore.swift:346, :371 | chat_voice_start, chat_voice_stop | none | ui-only (permission: microphone) |
| "Create project" | PlannerAgentView.swift:404; PlannerAgentStore.swift:327 | planning_accept | `work project-save` + `milestone-save` + `create` (several calls, not one atomic accept) | cli-ok (partial) |
| "Discard" / New plan | PlannerAgentView.swift:401; ChatView.swift:100; PlannerAgentStore.swift:176 | none | n/a | ui-only (runtime) |
| "Open project" after create | PlannerAgentView.swift:389 | none | `--project UUID` | cli-ok (read) |

## 3. Home (CaptureScreen.swift, Shell.swift; overview cards are in tasks.md "Today/Home", evening in meetings-notes-search.md §15)

| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| "Start recording" (dictation) | CaptureScreen.swift:131; AppModel.swift:769 | hud_toggle_recording | `sona --toggle-transcription` (to running app) | cli-ok |
| "Stop" | CaptureScreen.swift:156; AppModel.swift:777 | finish_recording | `sona --toggle-transcription` | cli-ok |
| "Cancel" (recording or working) | CaptureScreen.swift:162, :171; AppModel.swift:781 | cancel_operation | `sona --cancel` | cli-ok |
| start with post-processing | (hotkey only) | — | `sona --toggle-post-process` | cli-ok |
| start blocked: permission prompt | CaptureScreen.swift:132-136 | none | n/a | ui-only (permission: microphone, accessibility) |
| "Record the screen" | CaptureScreen.swift:143 | recorder sheet | n/a | ui-only (permission: screen capture) |
| "Import audio" | CaptureScreen.swift:149 | none (Settings > Importing) | `--transcribe-file PATH` transcribes without importing | ui-only (runtime: navigation) |
| recording shortcut chip → change shortcut | CaptureScreen.swift:112 | none (Settings > Essentials) | n/a | ui-only (runtime: navigation) |
| notice "Open in Library" / dismiss | CaptureScreen.swift:35, :41 | none | n/a | ui-only (runtime) |
| capture mode chip, "Edit modes" | CaptureScreen.swift:178; OverviewView.swift:424, :471 | get_modes, set_active_mode | MISSING | missing (see tasks.md) |
| "Count words per app" toggle (stats card) | OverviewView.swift:943; SettingsStore.swift:455 | change_count_words_per_app_setting | MISSING | missing |
| Home / Library tabs, sidebar places, `g h` | Shell.swift:179, :185, :440; ShortcutSheet.swift:16 | none | n/a | ui-only (runtime: navigation) |
| core failed: "Restart" / "Try again" | Shell.swift:95, :209; AppModel.swift:1008 | none (restarts core) | n/a | ui-only (runtime) |
| overview cards: today strip, loops, learning, updates, upcoming, series, stats, recents, evening | OverviewView.swift, EveningView.swift | see tasks.md rows 149-171 and meetings-notes-search.md §15 | — | already inventoried |

## 4. Notes and Search

Already inventoried in meetings-notes-search.md §16 (Notes, 22 rows) and §17 (Search, 8 rows). No new surfaces found: command palette (Panels.swift) and notes (ScratchpadView.swift) are covered there; cloud note share is in §10.

## 5. Settings, non-consent panes (meeting settings, connections, import and privacy are in meetings-notes-search.md)

#### Essentials
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load defaults/settings/accelerators/laptop | SettingsStore.swift:115-125 | get_default_settings, get_app_settings, get_available_accelerators, is_laptop | — | missing |
| microphone list load | SettingsStore.swift:139 | get_available_microphones | MISSING (`--list-devices` lists compute devices, not microphones) | missing |
| channel count load | SettingsStore.swift:148 | get_microphone_channels | — | missing |
| custom sounds load | SettingsStore.swift:153 | check_custom_sounds | — | missing |
| model capabilities load | SettingsStore.swift:158 | get_available_models | --list-models | cli-ok |
| Push-to-talk toggle | EssentialsView.swift:28 | change_ptt_setting | — | missing |
| Open Login Items | EssentialsView.swift:75 | (SMAppService) | — | ui-only (runtime: window) |
| Launch at login toggle | EssentialsView.swift:78 | change_autostart_setting + SMAppService login item | — | ui-only (permission) |
| Microphone Reset | EssentialsView.swift:110 | set_selected_microphone | — | missing |
| Microphone picker | EssentialsView.swift:114 | set_selected_microphone | --device-index (transcribe-file only) | missing |
| Auto detect any language toggle | EssentialsView.swift:196 | change_dictation_languages_setting | — | missing |
| Language checkbox | EssentialsView.swift:220 | change_dictation_languages_setting | — | missing |
| Languages popover Done | EssentialsView.swift:242 | — | — | ui-only (runtime: window) |
| Sound volume slider | EssentialsView.swift:285 | change_audio_feedback_volume_setting | — | missing |
| Sounds toggle | EssentialsView.swift:294 | change_audio_feedback_setting | — | missing |

#### Dictation
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Notice Dismiss | DictationSettingsView.swift:22 | — | — | ui-only (runtime: window) |
| Quiet speech toggle | DictationSettingsView.swift:35 | change_quiet_speech_enabled_setting | — | missing |
| Mute while recording toggle | DictationSettingsView.swift:49 | change_mute_while_recording_setting | — | missing |
| Command mode toggle | DictationSettingsView.swift:65 | change_command_mode_enabled_setting | — | missing |
| Command answers in chat toggle | DictationSettingsView.swift:75 | change_command_answers_in_chat_setting | — | missing |
| English spelling picker | DictationSettingsView.swift:92 | change_english_spelling_setting | — | missing |
| Translate to English toggle | DictationSettingsView.swift:100 | change_translate_to_english_setting | — | missing |
| Learn destination corrections toggle | DictationSettingsView.swift:109 | change_learn_destination_corrections_setting | — | missing |
| Overlay style picker | DictationSettingsView.swift:124 | change_overlay_style_setting | — | missing |
| Model unload timeout picker | DictationSettingsView.swift:138 | set_model_unload_timeout | — | missing |
| Experimental toggle | DictationSettingsView.swift:145 | change_experimental_enabled_setting | — | missing |
| Keyboard implementation picker | DictationSettingsView.swift:156 | change_keyboard_implementation_setting | — | missing |
| Project root Clear | DictationSettingsView.swift:183 | change_dictation_project_root_setting | — | missing |
| Project root Choose… (NSOpenPanel) | DictationSettingsView.swift:187 | change_dictation_project_root_setting | — | ui-only (privacy: folder grant via Finder panel) |
| Microphone channel picker | DictationSettingsView.swift:224 | set_selected_channel | — | missing |
| HUD pill Show now | DictationSettingsView.swift:252 | show_hud_pill_now | — | missing |
| Idle pill position picker | DictationSettingsView.swift:257 | set_hud_pill_position | — | missing |
| Show pill between dictations toggle | DictationSettingsView.swift:271 | set_hud_pill_enabled | — | missing |
| Transcribe accelerator/GPU picker | DictationSettingsView.swift:309 | change_transcribe_gpu_device, change_transcribe_accelerator_setting | — | missing |
| ORT accelerator picker | DictationSettingsView.swift:321 | change_ort_accelerator_setting | — | missing |
| Always-on microphone toggle | DictationSettingsView.swift:338 | update_microphone_mode | — | missing |
| Lazy stream close toggle | DictationSettingsView.swift:347 | change_lazy_stream_close_setting | — | missing |
| Clamshell mic Reset | DictationSettingsView.swift:373 | set_clamshell_microphone | — | missing |
| Clamshell microphone picker | DictationSettingsView.swift:379 | set_clamshell_microphone | — | missing |
| Sound theme picker | DictationSettingsView.swift:414 | change_sound_theme_setting (+check_custom_sounds) | — | missing |
| Test sound button | DictationSettingsView.swift:427 | play_test_sound | — | ui-only (runtime: audio output) |

#### Shortcut capture
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Reset shortcut | ShortcutCaptureRow.swift:45 | reset_binding | — | missing |
| Start capture (handy keys) | ShortcutCaptureRow.swift:130 | start_handy_keys_recording | — | ui-only (runtime: live keyboard) |
| Start capture (tauri) suspend | ShortcutCaptureRow.swift:135 | suspend_all_bindings | — | ui-only (runtime: live keyboard) |
| Commit chord | ShortcutCaptureRow.swift:227 | change_binding | — | missing |
| Finish/cancel capture | ShortcutCaptureRow.swift:246-247 | stop_handy_keys_recording / resume_all_bindings | — | ui-only (runtime: live keyboard) |

#### Display options
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Layout switch (list/board) | DisplayOptionsView.swift:23 | — (local draft; persisted via saved view) | work view-save | ui-only (runtime: navigation) |
| Grouping picker | DisplayOptionsView.swift:27 | — (local draft) | work view-save | ui-only (runtime: navigation) |
| Ordering picker | DisplayOptionsView.swift:39 | — (local draft) | work view-save | ui-only (runtime: navigation) |
| Empty groups toggle | DisplayOptionsView.swift:53 | — (local draft) | work view-save | ui-only (runtime: navigation) |
| Property chip toggle | DisplayOptionsView.swift:70 | — (local draft / myProperties) | work view-save | ui-only (runtime: navigation) |

#### Modes
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load | ModesView.swift:23 / ModesStore.swift:98-100 | get_modes, get_app_settings, get_available_models | --list-models (models only) | missing |
| provider key state load | ModesStore.swift:116 | get_provider_secret_state | — | ui-only (token) |
| Retry load | ModesView.swift:211 | get_modes, get_app_settings, get_available_models | — | missing |
| New mode | ModesView.swift:76 | upsert_mode (on save) | — | missing |
| Select mode row | ModesView.swift:109 | — | — | ui-only (runtime: navigation) |
| Drag reorder | ModesView.swift:148 | reorder_modes | — | missing |
| Move up/down (keyboard) | ModesView.swift:156-157 | reorder_modes | — | missing |
| Row menu Activate | ModesView.swift:189 | set_active_mode | — | missing |
| Row menu Duplicate | ModesView.swift:190 | upsert_mode | — | missing |
| Row menu Move up/down | ModesView.swift:191-192 | reorder_modes | — | missing |
| Row menu Delete → confirm | ModesView.swift:193,959 | delete_mode | — | missing |
| Delete Cancel | ModesView.swift:958 | — | — | ui-only (runtime: window) |
| Name field | ModesView.swift:251 | upsert_mode (on save) | — | missing |
| Save changes | ModesView.swift:253 | upsert_mode | — | missing |
| Cleanup enabled toggle | ModesView.swift:291 | upsert_mode (on save) | — | missing |
| Cleanup level picker | ModesView.swift:304 | upsert_mode (on save) | — | missing |
| ASR language picker | ModesView.swift:364 | upsert_mode (on save) | — | missing |
| Engine Local | ModesView.swift:385 | upsert_mode (on save) | — | missing |
| Engine cloud provider | ModesView.swift:388 | upsert_mode; consent sheet if needed | — | ui-only (consent) |
| Paste method picker | ModesView.swift:421 | upsert_mode (on save) | — | missing |
| Prompt preset picker | ModesView.swift:441 | upsert_mode (on save) | — | missing |
| Remove activation rule (app) | ModesView.swift:468 | remove_mode_activation_rule | — | missing |
| Remove activation rule (website) | ModesView.swift:468 | remove_mode_website_activation_rule | — | missing |
| Capture current app | ModesView.swift:492 | capture_mode_activation_rule | — | ui-only (runtime: frontmost window) |
| Website match scope picker | ModesView.swift:497 | — | — | ui-only (runtime: window) |
| Capture current website | ModesView.swift:507 | capture_mode_website_activation_rule | — | ui-only (runtime: frontmost window) |
| Shortcut Change (record) | ModesView.swift:631 | start_handy_keys_recording → change_binding | — | ui-only (runtime: live keyboard) |
| Shortcut Cancel recording | ModesView.swift:615,637 | stop_handy_keys_recording | — | ui-only (runtime: live keyboard) |
| Shortcut Reset | ModesView.swift:619 | reset_binding | — | missing |
| Tone picker | ModesView.swift:670 | upsert_mode (on save) | — | missing |
| LLM catalog auto-load | ModesView.swift:691,758 | discover_post_process_model_catalog | — | missing |
| Spoken instructions toggle | ModesView.swift:710 | upsert_mode (on save) | — | missing |
| LLM model ID field | ModesView.swift:732 | upsert_mode (on save) | — | missing |
| LLM catalog model pick | ModesView.swift:737 | upsert_mode (on save) | — | missing |
| Catalog Refresh | ModesView.swift:746 | discover_post_process_model_catalog | — | missing |
| Context policy picker | ModesView.swift:771 | upsert_mode (on save) | — | missing |
| Translate to English toggle | ModesView.swift:795 | upsert_mode (on save) | — | missing |
| Literal punctuation toggle | ModesView.swift:799 | upsert_mode (on save) | — | missing |
| Remove filler words toggle | ModesView.swift:800 | upsert_mode (on save) | — | missing |
| VAD toggle | ModesView.swift:801 | upsert_mode (on save) | — | missing |
| Clipboard handling picker | ModesView.swift:816 | upsert_mode (on save) | — | missing |
| Auto-submit toggle | ModesView.swift:817 | upsert_mode (on save) | — | missing |
| Auto-submit key picker | ModesView.swift:823 | upsert_mode (on save) | — | missing |
| Trailing space toggle | ModesView.swift:825 | upsert_mode (on save) | — | missing |
| Paste delay before/after | ModesView.swift:828-829 | upsert_mode (on save) | — | missing |
| Reliable paste toggle | ModesView.swift:835 | upsert_mode (on save) | — | missing |
| Local fallback toggle/model | ModesView.swift:851-853 | upsert_mode (on save) | — | missing |
| Cloud keyterms field | ModesView.swift:877 | upsert_mode (on save) | — | missing |
| Global vocabulary link | ModesView.swift:913 | — | — | ui-only (runtime: navigation) |
| Add vocabulary pair | ModesView.swift:914 | upsert_mode (on save) | — | missing |
| Vocabulary spoken/written fields | ModesView.swift:924,930 | upsert_mode (on save) | — | missing |
| Remove vocabulary pair | ModesView.swift:933 | upsert_mode (on save) | — | missing |
| Switch sheet Discard changes | ModesView.swift:988 | — | — | ui-only (runtime: window) |
| Switch sheet Keep editing | ModesView.swift:990 | — | — | ui-only (runtime: window) |
| Switch sheet Save and continue | ModesView.swift:994 | upsert_mode | — | missing |
| Consent Keep local | ModesView.swift:1034 | — | — | ui-only (consent) |
| Consent Accept and use cloud | ModesView.swift:1035 | accept_cloud_stt_provider_consent | — | ui-only (consent) |

#### Models
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Initial load: current + available models | AppModel.swift:1061 | get_current_model, get_available_models | --list-models | cli-ok |
| Rescan models button | ModelsScreen.swift:9 | rescan_local_models | — | missing |
| Retry load list | ModelsScreen.swift:27 | get_available_models (reloadModels) | --list-models | cli-ok |
| Cancel download | ModelsScreen.swift:92,117 | cancel_download | — | missing |
| Retry failed op | ModelsScreen.swift:97,138 | set_active_model / download_model | — | missing |
| Dismiss failure | ModelsScreen.swift:98 | (local state) | — | ui-only (runtime: window) |
| Use model | ModelsScreen.swift:106 | set_active_model | --model (transcribe-file only) | missing |
| Delete model | ModelsScreen.swift:107 | delete_model | — | missing |
| Download model | ModelsScreen.swift:120 | download_model | — | missing |

#### Providers
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Initial load settings | ProvidersView.swift:26; ProvidersStore.swift:94 | get_app_settings | — | missing |
| Auto-discover model catalog | ProvidersStore.swift:293 | discover_post_process_model_catalog | — | missing |
| Apple Intelligence availability check | ProvidersStore.swift:542 | check_apple_intelligence_available | — | missing |
| Local cleanup status load | ProvidersStore.swift:552 | get_local_cleanup_status | — | missing |
| Post-process enable toggle | ProvidersView.swift:92 | change_post_process_enabled_setting | --toggle-post-process (runtime toggle only) | missing |
| Download / retry local cleanup | ProvidersView.swift:129 | download_local_cleanup | — | missing |
| Cancel local cleanup download | ProvidersView.swift:120 | cancel_local_cleanup_download | — | missing |
| Use local cleanup for dictation | ProvidersView.swift:123 | select_local_cleanup | — | missing |
| Remove local cleanup (confirm) | ProvidersView.swift:73 | remove_local_cleanup | — | missing |
| Provider picker | ProvidersView.swift:207 | set_post_process_provider | — | missing |
| Base URL submit/Save | ProvidersView.swift:218,219 | change_post_process_base_url_setting | — | missing |
| API key Save | ProvidersView.swift:234 | set_provider_secret | — | ui-only (token) |
| API key Delete | ProvidersView.swift:242 | delete_provider_secret | — | ui-only (token) |
| Secret state read | ProvidersStore.swift:448 | get_provider_secret_state | — | ui-only (token) |
| Model choice menu item | ProvidersView.swift:300 | change_post_process_model_setting | — | missing |
| Refresh models | ProvidersView.swift:313 | discover_post_process_model_catalog | — | missing |
| Custom model submit/Use | ProvidersView.swift:330,331 | change_post_process_model_setting | — | missing |
| Provider consent: Allow text transfer | ProvidersView.swift:720 | accept_post_process_provider_consent | — | ui-only (consent) |
| Cloud STT key Save | ProvidersView.swift:521 | set_provider_secret | — | ui-only (token) |
| Cloud STT key Verify | ProvidersView.swift:525 | verify_stt_provider_secret, get_provider_secret_state | — | ui-only (token) |
| Cloud STT key Remove | ProvidersView.swift:530 | delete_provider_secret | — | ui-only (token) |
| Cloud STT consent: Accept and use cloud | ProvidersView.swift:778 | accept_cloud_stt_provider_consent | — | ui-only (consent) |
| Dictation prompt Use | ProvidersView.swift:589 | set_post_process_selected_prompt | — | missing |
| Dictation prompt Create/Update | ProvidersView.swift:656 | add_post_process_prompt / update_post_process_prompt | — | missing |
| Dictation prompt Delete (confirm) | ProvidersView.swift:56 | delete_post_process_prompt | — | missing |

#### Vocabulary
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Initial load settings/lists | VocabularyStore.swift:620,641,645,646 | get_app_settings, list_vocabulary_entries, list_snippets, get_text_replacements | — | missing |
| Load heard words | VocabularyView.swift:45 | vocabulary_candidates | — | missing |
| Keep spelling (heard word) | VocabularyView.swift:140 | learning_decide, add_vocabulary_correction, list_vocabulary_entries | — | missing |
| Dismiss heard word | VocabularyView.swift:138 | learning_decide | — | missing |
| Spoken edits toggle | VocabularyView.swift:91 | update_spoken_edits_enabled | — | missing |
| Emoji toggle | VocabularyView.swift:99 | update_emoji_replacements_enabled | — | missing |
| Snippets toggle | VocabularyView.swift:106 | set_snippets_enabled | — | missing |
| Rewrites toggle | VocabularyView.swift:113 | update_text_replacements_enabled | — | missing |
| Import CSV (file picker → preview) | VocabularyView.swift:167 | preview_vocabulary_csv | — | ui-only (runtime: Finder) |
| Import review Preview again | VocabularyView.swift:624 | preview_vocabulary_csv | — | missing |
| Import Continue/Back/Cancel | VocabularyView.swift:633,636,640 | (local state) | — | ui-only (runtime: window) |
| Apply import | VocabularyView.swift:643 | apply_vocabulary_csv | — | missing |
| Export CSV (save panel) | VocabularyView.swift:170 | export_vocabulary_csv | — | ui-only (runtime: Finder) |
| Restore default rewrites (confirm) | VocabularyView.swift:68 | reset_text_replacements | — | missing |
| Add rule kind picker | VocabularyView.swift:230 | (local state) | — | ui-only (runtime: window) |
| Add rule (submit/button) | VocabularyView.swift:247,258,261 | update_vocabulary_entries / update_emoji_replacements / save_text_replacements / upsert_snippet | — | missing |
| Edit rule field commit | VocabularyView.swift:491,500 | update_vocabulary_entries / update_emoji_replacements / save_text_replacements / upsert_snippet | — | missing |
| Rule enable toggle | VocabularyView.swift:506 | set_snippet_enabled / settings write | — | missing |
| Remove rule | VocabularyView.swift:208 | delete_snippet / list writes | — | missing |
| Persona samples load / Try again | VocabularyView.swift:320 | get_persona_samples | — | missing |
| Add/edit/remove sample | VocabularyView.swift:330,348; VocabularyStore.swift:593,608 | save_persona_samples | — | missing |

#### Prompts
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Initial load prompts | PromptsStore.swift:71 | saved_prompt_list | — | missing |
| Load dictation prompts | PromptsStore.swift:263 | get_app_settings | — | missing |
| Load targets (meetings) | PromptsStore.swift:197 | meeting_list | --meetings | cli-ok |
| Load targets (people) | PromptsStore.swift:212 | people_list | --people | cli-ok |
| Load targets (series) | PromptsStore.swift:225 | meeting_automation_roster | --roster | cli-ok |
| Library tab picker | PromptsView.swift:257 | (local state) | — | ui-only (runtime: navigation) |
| Target picker / clear | PromptsView.swift:141,150,200 | saved_prompt_runs | — | missing |
| New prompt | PromptsView.swift:79 | (opens editor) | — | ui-only (runtime: window) |
| Save prompt (editor) | PromptsView.swift:482 | saved_prompt_save / add_post_process_prompt / update_post_process_prompt | — | missing |
| Schema/target pickers in editor | PromptsView.swift:497,519 | (draft state) | — | ui-only (runtime: window) |
| Run prompt / Run again | PromptsView.swift:304,347 | saved_prompt_run | — | missing |
| Delete saved prompt | PromptsView.swift:310 | saved_prompt_delete | — | missing |
| Use dictation prompt | PromptsView.swift:421 | set_post_process_selected_prompt | — | missing |
| Delete dictation prompt | PromptsView.swift:429,603 | delete_post_process_prompt | — | missing |

#### Workflows
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Initial load workflows | WorkflowsStore.swift:348 | workflows_list | — | missing |
| Load runs first page / Retry | WorkflowsStore.swift:365; WorkflowsView.swift:58,247 | workflow_runs | — | missing |
| Show more runs | WorkflowsView.swift:64 | workflow_runs | — | missing |
| Load 7-day trend | WorkflowsStore.swift:404 | workflow_run_trend | — | missing |
| Workflow enable toggle | WorkflowsView.swift:107,133 | workflow_set_enabled | — | missing |
| Open run target | WorkflowsView.swift:78 | (navigation) | — | ui-only (runtime: navigation) |

#### Meeting prep
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| Initial load preferences / Read again | MeetingPrepView.swift:14,19 | meeting_prep_preferences_get | — | missing |
| Initial load web connection / Read again | MeetingPrepView.swift:20,134 | meeting_web_research_connection | — | missing |
| Initial load user voice | MeetingPrepView.swift:21 | voice_user_status | — | missing |
| About-me submit | MeetingPrepView.swift:64,202 | meeting_prep_preferences_set | — | missing |
| Learn my voice (consent dialog) | MeetingPrepView.swift:88,95 | voice_user_learn | — | ui-only (consent) |
| Forget my voice | MeetingPrepView.swift:84 | voice_user_forget | — | ui-only (privacy) |
| Check Mail | MeetingPrepView.swift:113 | meeting_mail_context_check | — | ui-only (permission) |
| Web research Save key | MeetingPrepView.swift:146 | meeting_web_research_key_set | — | ui-only (token) |
| Test saved key | MeetingPrepView.swift:155 | meeting_web_research_test | — | ui-only (token) |
| Remove key | MeetingPrepView.swift:159 | meeting_web_research_key_remove | — | ui-only (token) |
| Save preparation settings | MeetingPrepView.swift:186 | meeting_prep_preferences_set | — | missing |

#### Agent Bridge
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load status/sessions/requests/pending | AgentBridgeStore.swift:92-95 | get_agent_bridge_status / _sessions / _requests / _pending_messages | — | missing |
| initial load settings | AgentBridgeStore.swift:301 | get_app_settings | — | missing |
| load hook snippet | AgentBridgeStore.swift:318 | get_agent_bridge_hook_snippet | — | ui-only (token) |
| agent changes autonomy picker | AgentBridgeView.swift:51 | change_work_agent_changes_setting | — | ui-only (security) |
| Sona changes autonomy picker | AgentBridgeView.swift:59 | change_work_sona_changes_setting | — | ui-only (security) |
| Authorize folder | AgentBridgeView.swift:70 | authorize_agent_bridge_project | — | ui-only (security) |
| Remove project | AgentBridgeView.swift:82 | remove_agent_bridge_project | — | ui-only (security) |
| Copy setup code | AgentBridgeView.swift:101 | (pasteboard) | — | ui-only (runtime: pasteboard) |
| Remove rule | AgentBridgeView.swift:198 | delete_agent_bridge_permission_rule | — | ui-only (security) |
| master toggle | AgentBridgeView.swift:208 | set_agent_bridge_master | — | ui-only (security) |
| per-agent toggle | AgentBridgeView.swift:212 | set_agent_bridge_agent_enabled | — | ui-only (security) |
| Allow/Deny exactly this | AgentBridgeView.swift:252,254 | create_agent_bridge_permission_rule + respond_agent_bridge_permission | — | ui-only (consent) |
| Dismiss request | AgentBridgeView.swift:258 | dismiss_agent_bridge_request | — | ui-only (consent) |
| Confirm exact reply | AgentBridgeView.swift:285 | confirm_agent_bridge_reply | — | ui-only (consent) |
| Cancel pending | AgentBridgeView.swift:289 | cancel_agent_bridge_message | — | ui-only (consent) |
| reply session picker | AgentBridgeView.swift:313 | (local) | — | ui-only (runtime: navigation) |
| Create preview | AgentBridgeView.swift:342 | create_agent_bridge_reply_preview | — | missing |

#### Agent Pairing
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load settings | ChatStore.swift:~1123 | get_app_settings | — | missing |
| initial load / Retry identity | ChatStore.swift:~1131; AgentPairingView.swift:144 | agent_panel_public_identity | --agent-panel-public-identity | cli-ok |
| enable toggle | AgentPairingView.swift:28 | change_agent_panel_enabled_setting | — | ui-only (security) |
| Save / field submit | AgentPairingView.swift:96,226 | set_agent_panel_pairing | — | ui-only (token) |
| Test | AgentPairingView.swift:99 | agent_panel_test_connection | — | missing |
| Unpair | AgentPairingView.swift:102 | clear_agent_panel_pairing | — | ui-only (token) |
| Copy identity | AgentPairingView.swift:160 | (pasteboard) | — | ui-only (runtime: pasteboard) |

#### Cloud Sync
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load overview/service/statuses | CloudSyncStore.swift:129,144,159 | cloud_sync_overview_get / cloud_sync_service_status / cloud_sync_meeting_status_list | — | missing |
| initial load titles | CloudSyncStore.swift:176 | meeting_list | --meetings | cli-ok |
| Manage links | CloudSyncView.swift:25 | (navigation) | — | ui-only (runtime: navigation) |
| Pause/Resume sync | CloudSyncView.swift:67 | cloud_sync_pause / cloud_sync_resume | — | missing |
| Copy code / Copy link | CloudSyncView.swift:126,411,561 | (pasteboard) | — | ui-only (runtime: pasteboard) |
| Set up sync | CloudSyncView.swift:173 | cloud_sync_bootstrap | — | ui-only (token) |
| Recover / Replace vault | CloudSyncView.swift:189,515 | cloud_sync_recover | — | ui-only (token) |
| Keep current vault | CloudSyncView.swift:514 | (local) | — | ui-only (runtime: window) |
| Create pairing offer | CloudSyncView.swift:208 | cloud_sync_pairing_offer | — | ui-only (token) |
| Finish joining | CloudSyncView.swift:216,565 | cloud_sync_pairing_accept | — | ui-only (token) |
| Import .sona | CloudSyncView.swift:232 | cloud_share_import_file | — | missing |
| Paste candidate | CloudSyncView.swift:618 | cloud_sync_pairing_fingerprint | — | ui-only (token) |
| Approve device | CloudSyncView.swift:621 | cloud_sync_pairing_approve | — | ui-only (security) |
| Open meeting | CloudSyncView.swift:316 | (navigation) | — | ui-only (runtime: navigation) |
| Retry sync | CloudSyncView.swift:320 | cloud_sync_retry | — | missing |
| Keep device / Use cloud copy | CloudSyncView.swift:325,330 | cloud_sync_conflict_resolve | — | missing |
| expiry/include pickers | CloudSyncView.swift:343,353 | (local) | — | missing |
| Export .sona | CloudSyncView.swift:363 | cloud_share_create | — | missing |
| Create browser share | CloudSyncView.swift:366 | cloud_browser_share_create | — | ui-only (privacy) |
| expand row | CloudSyncView.swift:385 | cloud_sync_meeting_status_get + cloud_share_list | — | missing |
| Revoke share | CloudSyncView.swift:443,485 | cloud_share_revoke | — | ui-only (privacy) |

#### Documents
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load | ImportStore.swift:235 | doc_list | — | missing |
| Add document | ImportView.swift:221 | doc_ingest | — | missing |
| Delete document | ImportView.swift:252,274 | doc_delete | — | missing |

#### About
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load | AboutStore.swift:80,95-97 | get_app_settings / get_app_dir_path / get_log_dir_path / is_portable | — | missing |
| language/theme/material pickers | AboutView.swift:23,32,45 | change_app_language_setting / change_theme_setting / change_appearance_material_setting | — | missing |
| update check / what's new toggles | AboutView.swift:59,68 | change_update_check_enabled_setting / change_show_whats_new_on_update_setting | — | missing |
| Open repository / View release | AboutView.swift:95,204 | (NSWorkspace) | — | ui-only (runtime: Finder) |
| License / app data / logs / recordings | AboutView.swift:100,118,121,125 | open_license_notices / open_app_data_dir / open_log_dir / open_recordings_folder | — | ui-only (runtime: Finder) |
| Check now / Retry | AboutView.swift:152,200 | check_for_updates | — | missing |

#### Debug
| UI action | Swift file:line | bridge method | CLI | class |
|---|---|---|---|---|
| initial load | DebugStore.swift:204,213 | get_app_settings / get_default_settings | — | missing |
| model load | DebugStore.swift:264,270 | get_current_model / get_available_models | --list-models | cli-ok |
| log level picker | DebugView.swift:58 | set_log_level | — | missing |
| debug mode toggle | DebugView.swift:73 | change_debug_mode_setting | — | missing |
| recording buffer slider/Reset | DebugView.swift:112,118 | change_extra_recording_buffer_setting | — | missing |
| threshold slider/Reset | DebugView.swift:161,168 | change_word_correction_threshold_setting | — | missing |
| keyboard diagnostic Retry | DebugView.swift:211 | run_keyboard_diagnostic | MISSING (`--diagnose` diagnoses a `--transcribe-file` decode, not the keyboard) | missing |
| follow logs | DebugView.swift:324 | get_log_dir_path | — | missing |
| filter / Pause / Clear | DebugView.swift:273,285,290 | (local) | — | ui-only (runtime: window) |
| Copy logs | DebugView.swift:287 | (pasteboard) | — | ui-only (runtime: pasteboard) |
