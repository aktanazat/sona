# Sona state machines

The states Sona can be in, the rules that must hold in each, and the transitions between them, for the five parts where real failures came from: the dictation lifecycle, transcription, meeting recording, meeting notes and the ledger, and settings. Each part also lists the events that arrive out of band: a second key press, a cancel, a quit, a crash, a model switch, a setting changed mid-run.

Mapped at 4d1a0dc9. Every suspected bug went to a second reviewer who had not found it and whose job was to disprove it; a bug counts as confirmed only with a failing run or a trace with no unread branch. Each fix has a test that fails without it, except where the fix column says otherwise. Paths are relative to `src-tauri/src` unless they start with `macos/` or `src-tauri/swift/`. Line numbers drift; function names are the stable reference.

Each part has two sections. "After the audit fixes" is the current machine. "As mapped" is the machine the bugs were found in, kept because the candidate ids point into it.

Result: 86 candidates. 55 confirmed, 19 rejected, 5 unreachable in the shipped app, 7 needing evidence. The reviewers found 8 more (N1-N7, E16a).

## Confirmed

| id | part | bug | verdict | fix |
|---|---|---|---|---|
| A1 | Dictation lifecycle | A finished dictation's late "idle" hides the next live recording | confirmed, P1 in some setups and a race under defaults | fixed: the finish signal names its run and lives until the paste ran; only the coordinator writes idle |
| A2 | Dictation lifecycle | Cancelling during transcription leaves the core busy and swallows the retake | confirmed, P2 | fixed: a cancel ends processing at once; a late finish from the abandoned run is ignored |
| A3 | Dictation lifecycle | A crash during transcription leaves an unseen copy of the recording that nothing deletes | confirmed, P2 | fixed: recovery and retry audio matched to what history rows hold; no orphan copy |
| A4 | Dictation lifecycle | A stale "0" in "Dictations to keep" deletes history | sequence 1 rejected; sequence 2 confirmed but rare, P2 for the outcome | fixed: the limit field never saves a placeholder 0 |
| A5 | Dictation lifecycle | No keyboard cancel in hands-free mode with a key-only shortcut | confirmed, but in a narrower setup than stated; P3 | fixed: Escape stays registered as cancel when every shortcut is a bare key |
| A6 | Dictation lifecycle | Holding the key through transcription latches the next recording open | confirmed, P2 | fixed: a tap is timed key to key, so a held release stops instead of latching |
| A7 | Dictation lifecycle | A failed history save brings a pasted dictation back as "didn't finish" | Confirmed, for the crash-after-save variant only. The main sequence is rejected. | fixed: recovery matches the saved row, so a crash after save leaves no second "didn't finish" row |
| A9 | Dictation lifecycle | Retrying with history off does all the work, then throws it away | Confirmed. P2. | fixed: replay is refused up front when history is off; the buttons say why |
| A10 | Dictation lifecycle | A sweep during a retry leaves the new version with no audio | Confirmed. Low likelihood, P3. | fixed: a retry row shows no player for audio a sweep removed |
| B1 | Transcription | A streaming dictation fails outright when the engine stays busy for 30 s after stop | confirmed | fixed: a stream stopped behind a busy engine waits for the engine instead of failing at 30 s |
| B2 | Transcription | With a long vocabulary, the oldest entries silently stop working | confirmed (the mechanism is right; the stated cost is too strong) | fixed: the prompt is trimmed to whole entries; skipped entries keep fuzzy correction |
| B3 | Transcription | Echo detection misses echoes that do not begin at the prompt's first word | case A confirmed (it follows from B2); case B needs evidence | case A fixed with B2; case B needs a test build that prints per-segment text |
| B4 | Transcription | The room-noise recovery never runs with VAD off, or on a quiet capture that VAD called silent | case A confirmed, corrected; case B is B5; case C rejected | case A fixed: the room-noise recovery runs with VAD off |
| B5 | Transcription | Heard speech that decodes to nothing ends with no feedback | confirmed for "VAD forwarded speech, the model produced nothing" | fixed: speech that decodes to nothing is a failed capture kept for retry, with a message |
| B6 | Transcription | Starting a dictation or retry during another decode reloads the same model | confirmed | fixed: one engine slot records its build; a second load of the same model is skipped |
| B9 | Transcription | Clipboard restore keeps only plain text or one image | confirmed | fixed: the paste restores every clipboard item and type, including the concealed marker |
| B10 | Transcription | Paste and auto-submit go to whatever app is in front at delivery time | confirmed | fixed: delivery is held if the target window changed; text kept in the Library and offered as "Paste here" on the pill, never submitted |
| B13 | Transcription | Switching models mid-recording reloads the old model at stop | confirmed (low cost: slower paste, two wasted loads) | fixed: a lease keeps the in-use model; switching models waits for it |
| B14 | Transcription | An accelerator change is honored only by dictation and retry | confirmed (moderate) | fixed: the engine build includes the accelerator, so imports, meetings and chat voice follow it |
| B15 | Transcription | Deleting the model an in-flight recording uses fails that dictation | confirmed, but only through a narrower path that goes through B13 | fixed with B13 |
| B18 | Transcription | Retry reports "Recording contains no speech" when only post-processing emptied the text | confirmed (P3, misleading message) | fixed: a replay emptied by cleanup saves the empty text |
| C1 | Meeting recording | Deleting any meeting during a recording drops that recording's keep-awake and opens the processing gate | confirmed | fixed: the capture hold is released only by the capture that took it |
| C2 | Meeting recording | A synced meeting deleted during any recording is never removed from the cloud | confirmed (separate defect, same trigger as C1) | fixed: a mid-capture delete queues its cloud removal |
| C3 | Meeting recording | Any native failure during a screen recording deletes the whole recording | confirmed (trigger corrected) | fixed: a native failure saves the recording with a note on what cut it short |
| C4 | Meeting recording | Quitting during a meeting cuts it without warning and leaves it unfinished | confirmed (product gap) | fixed: quit during a meeting asks first; Stop and Quit keeps the audio for notes at next launch |
| C5 | Meeting recording | Quitting during a screen recording loses it, and may leave a hidden file forever | confirmed | fixed: quit asks first; Stop and Quit saves; launch removes hidden partial files |
| C6 | Meeting recording | A lane's writer thread dies unnoticed, then Stop wedges the meeting in Stopping | confirmed (rare trigger: a storage or disk error mid-capture, realistically a full disk) | fixed: Stop and Discard finish over a dead lane writer |
| C7 | Meeting recording | A meeting accepted from a notification loses its auto-stop | confirmed | fixed: detection tracking follows the session phase, so prompt-started captures keep their stop rules |
| C8 | Meeting recording | A refused auto-stop or recording-card stop is treated as a stop | confirmed (low frequency) | fixed: tracking ends only when a stop commits; a refused stop is retried |
| C9 | Meeting recording | Resume after a failed resume can never succeed, and a failed lane stays dead | confirmed | fixed: Resume is offered only when a lane can resume |
| C10 | Meeting recording | A start that fails part-way leaves the meeting stuck in Starting | confirmed in part (the leak is real; the "dead Record button" claim is rejected) | fixed: a start that fails part-way aborts back to Preflight and releases its leases |
| C11 | Meeting recording | Finishing an unfinished meeting during a recording clears the processing gate | confirmed (the gate half of C1's root cause) | fixed with C1 |
| C12 | Meeting recording | If the screen recorder sheet is replaced while paused, the recording can't be reached until quit | confirmed (the user loses a lot, but only a few things trigger it) | fixed: reopening the recorder shows the held recording |
| C13 | Meeting recording | Quitting or crashing during an import leaves a truncated "unfinished" meeting, and phone recordings come back twice | confirmed (medium) | fixed: an import cut off by quit or crash is discarded at launch |
| C17 | Meeting recording | A startup recovery failure is never retried | confirmed, with a different trigger than the candidate's | fixed: startup recovery runs inside the store mount |
| C18 | Meeting recording | A pause or resume that fails part-way leaves the meeting in Pausing or Resuming | confirmed (low; needs a failed database write) | fixed: a failed pause or resume rolls back to the prior phase |
| C19 | Meeting recording | The tray's "Cancel Meeting Notes" discards with one click, during Processing too | confirmed (in the Mac app only through N1; fully reachable on tray builds) | fixed: Cancel Meeting Notes is offered only before capture ends |
| D1 | Meeting notes and ledger | "Try again" on an untitled meeting writes the notes, then reports a conflict and keeps "Notes failed" | confirmed | fixed: an untitled meeting's retry keeps its notes and status |
| D2 | Meeting notes and ledger | Any short action clears "busy" mid-rewrite, so a second paid rewrite runs alongside the first | confirmed | fixed: one rewrite at a time, held by the job registry, not the busy flag |
| D3 | Meeting notes and ledger | The ledger tab and the loops come from different revisions | confirmed | fixed with D9: the tab and the core use one rule |
| D4 | Meeting notes and ledger | A successful rewrite never runs the "notes are ready" follow-ons | confirmed, with one correction | fixed: a first successful retry runs the in-app follow-ons once; sends and automations do not fire |
| D5 | Meeting notes and ledger | Turning remote intelligence off mid-run does not stop uploads | confirmed | fixed: cloud AI off or keep-local stops a running job at its next request |
| D6 | Meeting notes and ledger | One timed-out join throws away the structured notes and skips the ledger | confirmed, narrowed to the skipped ledger | fixed: plain notes after a time limit still get a ledger |
| D7 | Meeting notes and ledger | One failed ledger merge throws away every ledger part | confirmed for the mechanism; the likelihood claim is wrong | fixed: every ledger part is kept when they cannot be joined, marked as not joined |
| D9 | Meeting notes and ledger | Any edit hides the meeting's loops everywhere until a paid rewrite | confirmed | fixed: loops read the newest notes revision that has a ledger, current first |
| D12 | Meeting notes and ledger | The ledger merge drops an answered open question unless the citations match exactly | confirmed (the mechanism differs from the candidate's claim) | fixed: an answer in a later part closes the question |
| D15 | Meeting notes and ledger | A quit between storing notes and `finish_review` lands the meeting in recovery | confirmed (rare; the window is milliseconds) | fixed: a launch after notes were stored enters review without rewriting; sends fire only where a re-run would have |
| D16 | Meeting notes and ledger | The record-page engine warning goes stale while Sona stays in front | confirmed (low severity; the "during capture" part is rejected) | fixed: the meetings home rechecks the notes engine every 30 s |
| E1 | Settings and migrations | A torn or empty settings file silently becomes a fresh install, and the startup cleanup then trims history | confirmed | fixed: saves are atomic; an unreadable file is kept aside and that run deletes no history |
| E2 | Settings and migrations | Re-importing Handy history after Handy was used again imports every entry and recording a second time | confirmed (same root cause as E3) | fixed: re-import matches by entry, not by the legacy app's identity, so nothing doubles |
| E3 | Settings and migrations | The same identity change orphans the settings-import undo and re-arms the settings import over later edits | confirmed (same root cause as E2) | fixed with E2: the settings-import undo survives |
| E5 | Settings and migrations | Importing from Handy deletes Sona history at the next dictation, with no warning | confirmed for sequence B; sequence A is a design choice | fixed: an import that would push out older dictations says how many first and runs only after "Import anyway" |
| E9 | Settings and migrations | A failed first entry blocks every later "Move data" | confirmed (rare trigger, but once hit it never recovers) | fixed: a failed move can run its plan again |
| E12 | Settings and migrations | Bookkeeping writes advance the settings generation and break agent-panel changes | confirmed (common for users with cloud cleanup) | fixed: the verified-key stamp no longer moves the settings revision |
| E13 | Settings and migrations | Import and revert write the receipt before the settings are saved | confirmed (low severity; needs a crash in a short window, or a save failure that lasts through quit) | fixed: the receipt is written only after the settings are saved |
| N1 | Agent panel | Approving any chat review card (mail draft, Slack post, calendar event, notes send) hangs the chat until quit | confirmed | fixed: the command reads the chat panel under the type the app manages |
| N2 | Settings and identity | The move-your-data prompt deadlocks the core at startup for anyone with old data | confirmed (reproduced with a scratch home) | fixed: the move question is asked without blocking startup |
| N3 | Meeting recording | Chat settings can show the hidden Rust tray inside the Mac app | confirmed | fixed: the Mac app never builds the Rust tray |
| N4 | Transcription | Whisper dictations over 30 s get exact-only vocabulary correction past the first 30 s | confirmed | fixed: fuzzy correction applies past the first 30 s |
| N5 | Transcription | A whisper phantom phrase on a short silent capture is pasted | confirmed (mechanism; frequency unmeasured) | fixed: a short capture that decodes only to a known whisper phantom is silence |
| N6 | Dictation | Cancelling the next recording before delivery skips pasting the earlier dictation | confirmed | fixed with A1: the earlier paste runs before the next recording opens |
| N7 | Dictation | Library says "Encrypted at rest" while history is locked; Privacy says unencrypted | confirmed | fixed: the storage line follows the lock state |
| E16a | Settings and migrations | A chat Apply whose save fails says "Not applied" while the change is live | confirmed | fixed: a chat change whose save fails stays undoable |

## Needs evidence

| id | part | bug | verdict | fix |
|---|---|---|---|---|
| A11 | Dictation lifecycle | A slow microphone start turns a tap-to-latch into a stop | Needs evidence. The mechanism is confirmed in code. Same root cause and fix as A6. | fixed with A6 (release time comes from the key edge); hardware timing unmeasured |
| B7 | Transcription | The redo without the prompt can turn noise into a made-up phrase | needs evidence | open: the whisper half is measured (see N5); whether voice detection forwards 5 s or more of non-speech is not |
| B12 | Transcription | Window cuts in steady room noise fall mid-word | needs evidence (disproven for this week's whisper failure) | open: needs SenseVoice, GigaAM or Moonshine weights, not on this Mac |
| C14 | Meeting recording | A paused, detection-tracked capture auto-stops across sleep | needs evidence (a product decision; the code trace is complete) | owner decision: a paused capture is not stopped by sleep; it waits for Resume or Stop |
| C16 | Meeting recording | The startup sweep races the first commands | needs evidence (a timing window; the code has no guard) | closed by C17: recovery runs before the first command can reach the store |
| E6 | Settings and migrations | Undoing the identity move is reversed by Sona's own quit, and the move is never offered again | needs evidence (can it be reached?); everything after a completed move checks out | fixed: Sona no longer writes settings back on quit, so putting data back stays put |
| E15 | Settings and migrations | Salvage fills missing keys with fresh-install values instead of upgrade values | needs evidence | open: needs a legacy settings file to test; the owner's real one was not read |

## Unreachable in the shipped app

| id | part | bug | verdict | fix |
|---|---|---|---|---|
| D11 | Meeting notes and ledger | The backend rewrite has no phase or job guard | unreachable in the shipped app | not changed; the rewrite job registry from D2 now guards it |
| E7 | Settings and migrations | An interrupted move from the old app identity is never resumed, because the journal is skipped | unreachable in the shipped app | fixed: a move cut short resumes from where it stopped |
| E8 | Settings and migrations | CLI and agent processes run the identity move and its dialogs | unreachable in the shipped app as written | fixed: only the app asks the move question; CLI and agent runs never answer it |
| E10 | Settings and migrations | An older build silently rewrites a newer settings file, including the older installed copy the agent tool server runs | unreachable in the shipped app | not changed; unreachable as shipped |
| F1 | Agent panel | An event made from the agent panel can lose its undo | unreachable in the shipped app today (for calendar events); the race is real and becomes live once N1 is fixed | fixed: nothing replaces the chat while a card is applied, undone or edited |

## Rejected

| id | part | bug | verdict |
|---|---|---|---|
| A8 | Dictation lifecycle | A Library retry of a long recording blocks live dictation, with no way to stop it | Rejected: the wait is the designed first-come order. Duplicate of A2's lease cost. |
| A12 | Dictation lifecycle | "Finish and start a meeting" plus a core crash tries to start a meeting on a dead core | Rejected: harmless. |
| B8 | Transcription | One recovered span switches the whole capture to fuzzy correction and turns off the whole-text echo check | rejected |
| B11 | Transcription | A failed paste chord is reported as "Sent", and the transcript is taken off the clipboard | rejected |
| B16 | Transcription | If the stream fails to begin, the worker holds the gate for the rest of the recording | rejected (harmless) |
| B17 | Transcription | A new unload timeout is not applied until the next activity | rejected |
| B19 | Transcription | A meeting chunk made only of vocabulary words is dropped | rejected (documented design) |
| C15 | Meeting recording | A calendar-linked capture stops at the scheduled end while the call is still live | rejected (intended), but the product question remains |
| C20 | Meeting recording | The tray's stop never ends detection tracking | rejected (harmless; in the Mac app it also needs N1) |
| C21 | Meeting recording | A stale revision makes the user press twice | rejected (harmless; the code expects it) |
| D8 | Meeting notes and ledger | A worse rewrite replaces complete notes with no way back | rejected, designed behaviour |
| D10 | Meeting notes and ledger | One failed 2 s context probe sets the budget to 0 | rejected (deliberate, locked by a test) |
| D13 | Meeting notes and ledger | A failed rewrite turns the status to Failed over intact notes | rejected (deliberate) |
| D14 | Meeting notes and ledger | A cancel during notes is recorded as a completed run | rejected (harmless; latent only) |
| E4 | Settings and migrations | "Undo the settings import" restores the whole pre-import document, not just what the import changed | rejected (intended) |
| E11 | Settings and migrations | Nothing reloads the file, so hand edits and other writers are silently lost | rejected |
| E14 | Settings and migrations | The schema-20 step overrides a history limit of 5 that the user chose | rejected |
| E16 | Settings and migrations | A failed save leaves the app running on a value the disk does not have | rejected (intended and pinned) |
| F2 | Agent panel | Calendar events from the agent panel can be made twice | rejected |

## Dictation lifecycle

### After the audit fixes

#### M1. Dictation coordinator
Owner: `transcription_coordinator.rs`. Loop 616-666, `execute` 743-781, `begin` 784-801, `start` 805-828.

State:
- `Stage` (155-172): Idle, Recording{intent, run_plan, pressed_at, latched, duration, suggestion}, or Processing{run_id}
- `pending_press` (87-95): {intent, shortcut_label, pressed_at, latched}
- `pending_release` (76-81): {intent, Stop or Latch, deadline}
- `last_press`, for the 30 ms debounce
- every input carries `at`, the time its edge was sent (127-139; stamped at 686 and 700)

Invariants:
- I1: Recording means the audio manager accepted a session: `start` gets the answer from `start_transcription` itself (822-827).
- I2: Processing{run_id} waits for exactly that run's `FinishGuard`, which lives until C1 has run (actions.rs 207-223, 2026-2029, 2312-2314). A finish for another run is ignored (517-520).
- I3: a remembered press exists only in Processing (389-392).
- I4: a dictation's final overlay and tray idle come only from Settled (757-763), so they cannot land over a newer recording.

| from | event | guard | to / effect | path:line |
|---|---|---|---|---|
| Idle | key press (PTT or toggle) | more than 30 ms since last press | Start{pressed_at = edge time, latched=false}; Recording/Held if `start` returns a plan, else Idle | 346-429 (debounce 378-387), 208-215, 784-801, 805-828 |
| Idle | pill, HUD, tray or CLI start (`send_intent`, external, no debounce) | — | Start{pressed_at = edge time, latched=true}; Recording/Latched; bare cancel key released | 693-702, 798-800 |
| Recording/Held | PTT release | tapped (edge time − pressed_at under 400 ms) | adds grace(Latch, 50 ms) | 348, 326-344, 361-369 |
| Recording/Held | PTT release | not tapped | adds grace(Stop, 50 ms) | 348, 326-344, 361-369 |
| Held + grace | same-intent press | — | grace cleared (auto-repeat absorbed) | 357-360 |
| Held + grace(Latch) | 50 ms timer | still recording that intent | Recording/Latched + Effect::Latched (`release_bare_cancel_key`) | 315-319, 478-501, 770 |
| Held + grace(Stop) | 50 ms timer | same intent | Processing{run_id} + Stop | 478-484, 572-602 |
| Recording/Held | PTT press, same intent | — | nothing (OS repeat) | 402 |
| Recording/Latched | PTT press, same intent | — | Processing{run_id} + Stop | 398-400 |
| Recording (either) | toggle press, same intent | — | Processing{run_id} + Stop | 412-414 |
| Recording | press for another intent | — | ignored | 406, 418-424 |
| Recording | Finish (pill Stop, tray, `finish_recording`) | — | Processing{run_id} + Stop | 562-568 |
| Recording | 3 min / 19 min / 20 min, on `Instant` | — | MeetingSuggestion (once) / DurationWarning / Finish("duration-limit") | 296-314 |
| any | Cancel | — | Idle; remembered press and grace cleared | 505-511, loop 644-651 |
| Processing | PTT press | — | remember {pressed_at = edge time} | 389-392, 433-473 |
| Processing | PTT release | a press is remembered | forget | 433-473 |
| Processing | toggle press | nothing remembered / one remembered | remember / forget | 433-473 |
| Processing | Finish | — | ignored | 562-568 |
| Processing{run_id} | ProcessingFinished{run_id}, after C1 | press remembered | Idle; Settled(Start{pressed_at = the press's own time, latched = remembered}): overlay and tray idle, then begin | 516-535, 757-763 |
| Processing{run_id} | ProcessingFinished{run_id}, after C1 | nothing remembered | Idle; Settled(None): overlay and tray idle | 516-535, 757-763 |
| any other | ProcessingFinished{other run} | — | ignored (an abandoned run) | 517-520 |
| any | the coordinator thread panics | — | thread gone; later sends log "channel closed" | 616-666 |

Out-of-band events now:
- Cancel during Processing ends the stage; the abandoned pipeline finishes in the background and is ignored.
- Tap timing uses the edge's own time, so a release queued behind a slow start still measures the key. Debounce and the grace window still use the read time.
- The late paste now precedes the drain, so it cannot overlap a new recording.

#### M2. Audio recording state
Owner: `managers/audio.rs`.

| from | event | guard | to | path:line |
|---|---|---|---|---|
| Idle | try_start_recording | stream closed / already open | open the stream, then Recording{spool} plus a readiness thread / Recording almost at once | 1478-1544; actions.rs 1871-1931 |
| Recording | stop_recording | same binding, and no cancel since the stop was requested | readiness invalidated, Stopping (spool taken), extra buffer, `rec.stop()`, then Idle | 1719-1855 (guard 1726, invalidation 1727) |
| Recording | stop_recording | a cancel came since (an abandoned run's late stop) | untouched; returns no outcome | 1726, 1856-1860 |
| Stopping | stop outcome | cancelled / Err / overrun / VAD none / complete | None+spool / None+spool / Overrun / NoSpeech / Complete | 1777-1855 |
| Recording | cancel_recording | — | cancel_gen++, Idle, `rec.stop()`, unmute, close, return the spool | 1880-1921 |
| Stopping or Idle | cancel_recording | — | cancel_gen++ only; returns None | 1880-1921 |
| readiness thread | first samples | generation still current | "recording-ready", start sound, mute | actions.rs 1891-1931 |
| any | stop begins | — | capture_generation++, so a late ready signal or mute is dropped | actions.rs 1972; 1703 |

#### M3. Cancel keys
Owner: `shortcut/mod.rs`. State: `CANCEL_KEYS` None, Held or HandsFree (62-71, 74); `REGISTERED_CANCEL` (77-79).

Which keys are live:
- Held: the bare cancel key, plus one `cancel#{prefix}` for each dictation binding that has modifiers (the command binding only with command mode on) (237-262, 271-).
- HandsFree: the `cancel#` variants, and the bare key only if it has a modifier. When there is no variant, the bare key stays (111-122, guard at 116).

Invariant now: a recording always has a keyboard cancel while the cancel binding is set.

| from | event | to | path:line |
|---|---|---|---|
| None | a recording starts | Held if `push_to_talk`, else HandsFree | 81-89; actions.rs 1942 |
| Held | latch (a tap, or a latched Start) | HandsFree | 92-101; coordinator 770, 798-800 |
| any | stop | None | 104-107; actions.rs 1975 |
| any | cancel | None | utils.rs 82 |
| any change | — | a spawn_blocking sync to the newest wanted set | 124-185 |

#### M4. Stop pipeline and delivery
Owner: `actions.rs`, `TranscribeAction::stop` 1968-2437. Delivery (C1) runs on the core's main thread at 2313-2359.

Stages:
1. S0 stop requested: readiness invalidated (1972), cancel keys off (1975), tray Transcribing (1985), `cancel_generation` captured (2023)
2. S1 `stop_recording` (2036), with `FinishGuard` created at 2026-2029 and `SpoolClaim` at 2037-2041
3. S2 decode (2119-2136). It cannot be cancelled.
4. S3 no-speech and held-cloud checks
5. S4 rewrite, cancellable through `complete_unless_cancelled` (2249-2259)
6. S5 save the row (2295)
7. S6 queue C1 (2313) holding a clone of the guard (2312, 2314). The task ends: `SpoolClaim` drops first, then the task's guard.
8. S7 C1 delivery. When the last guard holder drops, ProcessingFinished{run_id} goes to the coordinator (213-223).

Invariants:
- No branch writes the overlay or tray idle; the coordinator does once the run, its paste included, is over (M1 I4).
- The spool's fate is decided before ProcessingFinished (`_spool` drops before `finish`).
- The row with the words is saved before delivery. Delivery is never retried.
- A cancel is honoured up to C1's own check (2315) and ignored after it.

| from | outcome / event | guard | to | path:line |
|---|---|---|---|---|
| S1 | NoSpeech | cancelled since gen | task ends; nothing saved; stream untouched | 2043-2047 |
| S1 | NoSpeech | — | stream torn down; no-speech row | 2048-2061 |
| S1 | Complete | cancelled | task ends | 2074-2077 |
| S1 | Complete | VAD silent, cloud-only | stream torn down; no-speech row | 2085-2102 |
| S1 | Complete | no samples | stream torn down; no row, no message | 2104-2108 |
| S1 | Complete | — | WAV written alongside (`sona-{stop second}-{run}.wav`, 1309), decode starts | 2109-2136 |
| S2 | Ok | cancelled | task ends | 2162-2165 |
| S2 | Err | cancelled / not cancelled | task ends / failed row + "transcription-error" | 2371-2403 |
| S3 | HeldCloudUnavailable / no speech confirmed | — | row | 2168-2196 / 2215-2241 |
| S4 | rewrite | cancel arrives | task ends | 2249-2259 |
| S4 | after rewrite | cancelled | task ends | 2261-2264 |
| S5 | final text empty | — | row + not_dispatched | 2283-2290 |
| S5 | text | — | row saved; C1 queued with a guard clone | 2295, 2312-2313 |
| S1 | Overrun | cancelled / not | task ends / stream torn down, truncated prefix row | 2407-2421 |
| S1 | None | cancelled since gen / not | stream untouched / stream torn down; no row | 2422-2429 |
| S7 | C1 starts | `was_cancelled_since(gen)` | not_dispatched | 2315-2326 |
| S7 | C1 | — | `deliver`, delivery event, paste-error, receipt | 2328-2359 |
| S6 | scheduling fails | — | not_dispatched; the closure and its guard clone drop | 2360-2368 |
| any | the last guard holder drops (task end, C1 end, or a panic) | — | ProcessingFinished{run_id} → M1 Settled | 213-223 |

#### M5. Spool and crash recovery
Owners: `actions.rs` (`SpoolClaim` drop 1429-1459, `keep_cancelled_capture` 1463-1496) and `managers/history.rs` (`spool_path` 1132-1138, `recover_interrupted_dictations` 1459-1532, `settle_interrupted_with_connection` 1540-1579, `list_interrupted` 3327-3355). Startup calls recovery at lib.rs 2374.

Invariants, both now held across a crash:
- J1: a spool never sits beside a row that holds its run receipt.
- J2: every file in recordings is referenced by a row.

| from | event | guard | to | path:line |
|---|---|---|---|---|
| — | start | — | Writing (`sona-{start second}-{run}.wav`) | 1132-1138; actions.rs 1871-1875 |
| Writing | cancel_recording | under 10 s / 10 s or more | Removed / Kept as Cancelled | utils.rs 86; actions.rs 1463-1496 |
| Finished | SpoolClaim drop | receipt exists / cancelled since gen / limit 0 / otherwise | Removed / Kept as Cancelled / Removed / Left | actions.rs 1429-1459 |
| Left or crash orphan | launch | listed when HistoryManager is built, with the copies a decode left in recordings (`sona-{T}-{R}.wav`, T ≥ the spool's start second) | queued | 811, 3327-3355 |
| queued copy | recovery | no row holds it / a row does | Removed / kept | 1545-1551 |
| queued spool | recovery | a receipt for run R started that second exists / not | Removed / kept for recovery | 1552-1578 |
| queued | recovery | history unreadable (store locked) | every spool kept, copies untouched; warning | 1469-1477 |
| kept for recovery | recovery loop | repair error / under 1 s / `for_retry` error / save error / limit 0 / ok | Left / Removed / all Left / renamed back / file removed / Interrupted row | 1481-1531 |

#### M6. Retention
Owners: `managers/history.rs`, `commands/history.rs`.

Invariants:
- Kept rows are outside the count sweep.
- At limit 0 there are no new rows, and a replay is refused before any work.
- A derived row has audio only while a row with audio holds its file.
- A WAV that two rows share survives until both are gone (2048-2062, 2067-2104).

| from | event | guard | to | path:line |
|---|---|---|---|---|
| — | insert (`save_entry_with_receipt_with_connection`) | limit 0 | a non-derived file is removed and no row is written; a derived row gets Ok(None) | 1693-1763 |
| — | insert, derived row | no row with audio holds the file any more | row saved with `has_audio = false` | 1722-1724, 2048-2062 |
| — | insert | otherwise | row, then `cleanup_old_entries` | 1693-1763, 1979-2021 |
| cleanup | count sweep | unsaved, not kept, past the limit | rows and their files deleted | 2105-2137, 2067-2104 |
| cleanup | kept sweep / recording expiry | 14 days / retention period | deleted | 2186-2213 / 2138-2185 |
| — | `update_history_limit(n)` | — | the setting changes and cleanup runs at once | commands/history.rs 359-373 |
| — | replay | limit 0 | refused before any read | commands/history.rs 223, 239-241 |
| — | replay | limit set to 0 mid-run | refused at save | commands/history.rs 294 |

#### M8. Swift Library mirror
Owners: `macos/Sona/LibraryStore.swift` and `LibraryScreen.swift`.

State:
- `limit: Int?`, nil until the core's limit is read (101)
- `storage: StorageStatus?` (103)
- `retention`, `savingRetention`, `retrying`, `deleting`, `kept`
- the retention field's `LimitDraft` (LibraryScreen 1056; LibraryTypes 493-522) and `editingLimit`

Invariant: the limit on screen is the core's limit or nothing; a placeholder can never be saved.

| from | event | to | path:line |
|---|---|---|---|
| any | Library appears | `start()`: six requests in parallel | LibraryScreen 46; Store 149-157 |
| any | `loadSettings` succeeds / fails | limit = `historyLimit ?? 100` / limit unchanged (nil stays nil) plus an error | 635-643 |
| any | core restart | `coreStarted`: reloads settings and the kept list | 258-263 |
| field | limit unknown | disabled | LibraryScreen 1073 |
| field | appears / limit changes | the draft follows the limit unless the reader has typed | LibraryScreen 1091-1094; LibraryTypes 503-507 |
| field | submit or focus lost | saves only text the reader changed; bad text reverts | LibraryScreen 1095-1104; LibraryTypes 512-521 |
| store | `updateLimit(n)` | only if `next != limit`; request; then limit = next | 657-668 |
| row | Transcribe / Transcribe again / Process again | disabled while busy or while `limit == 0`, with the reason as help text | 651-653; LibraryScreen 324-327, 704-707, 716-717 |
| storage line | status read | `StorageStatus.line`: "Encrypted at rest" only when readable; locked says "...so history cannot be read." | LibraryScreen 1084-1088; PrivacyTypes 268-284 |

#### M9. Library retry and reprocess
Owner: `commands/history.rs` `replay_stored_recording` 232-303.

Flow:
1. Freeze a plan: `RunPlan::for_retry` (327) or `for_reprocess` (348)
2. History off: refused before anything is read (239-241)
3. Read the whole WAV (242-249)
4. One decode over the whole file, holding the FIFO engine lease, with no cancel (255-263)
5. Empty result → error (265-267)
6. Rewrite (269)
7. Save the derived row (271-294); a limit set to 0 mid-run is refused here (294); the row keeps audio only if a row with audio still holds the file (history.rs 1722-1724)
8. Delivery receipt `not_dispatched` (295-301)

### As mapped

#### M1. Dictation coordinator
Owner: `transcription_coordinator.rs`. It runs on one thread with one channel: the loop is at 600-650, `execute` at 721-768 and `start` at 772-795.

The state is four parts together:
- `Stage` (150-163): Idle, Recording{intent, run_plan, pressed_at, latched, duration, suggestion}, or Processing
- `pending_press`: a remembered press {intent, latched}
- `pending_release`: a grace window {intent, Stop or Latch, deadline}
- `last_press`, used for the 30 ms debounce

Named combinations:
- Idle
- Recording/Held
- Recording/Held + grace(Stop)
- Recording/Held + grace(Latch)
- Recording/Latched
- Processing
- Processing + remembered press. For a keyboard press the key is physically down. For an external start it is latched.

Combinations the types allow that nobody named:
- Processing + a stale grace window. The timer uses it up within 50 ms. `on_grace_expired` finds no Recording and has no effect.
- Idle after a drained Start whose `start()` returned no plan. The press is used up, nothing records, and the start path reports the refusal.

Invariants:
- I1: Recording means the audio manager accepted a session. `on_started` (524-543) runs only with the plan `start()` returns after `is_recording()` (784-786).
- I2: Processing means exactly one stop task exists. Its `FinishGuard` (actions.rs 2015, drop at 212-213) sends ProcessingFinished exactly once.
- I3: a remembered press exists only in Processing. `remember_or_forget` is only reached from Processing (376-379).
- I4 (across processes): the tray and `dictation-activity` mirror shows the same thing as Stage. A1 and A2 break this.

| from | event | guard | to / effect | path:line |
|---|---|---|---|---|
| Idle | key press (PTT or toggle) | more than 30 ms since last press | Start{pressed_at=now, latched=false}; Recording/Held if a plan returns, else Idle | 335-418 (debounce 365-374, match ~381-416), 721-795, 524-543 |
| Idle | pill, HUD, tray or CLI start (`send_intent`, external, no debounce) | — | Start{latched=true}; Recording/Latched; bare cancel key released | 192-199, ~666-674, execute Start |
| Recording/Held | PTT release | tapped (under 400 ms since pressed_at) | adds grace(Latch, 50 ms) | 201-234, 315-333 |
| Recording/Held | PTT release | not tapped | adds grace(Stop, 50 ms) | 201-234 |
| Held + grace | same-intent press | — | grace cleared (auto-repeat absorbed) | 201-234 CancelRelease |
| Held + grace(Latch) | 50 ms timer | still recording that intent | Recording/Latched + Effect::Latched (`release_bare_cancel_key`) | 282-312, 465-490 |
| Held + grace(Stop) | 50 ms timer | same intent | Processing + Stop | 465-471, 559-584 |
| Recording/Held | PTT press, same intent | — | nothing (treated as OS repeat) | ~383-394 |
| Recording/Latched | PTT press, same intent | — | Processing + Stop | ~383-394 |
| Recording (either) | toggle press, same intent | — | Processing + Stop | ~397-411 |
| Recording | press for another intent | — | ignored | ~392, ~405-410 |
| Recording | Finish (pill Stop, tray, `finish_recording`) | — | Processing + Stop | 549-555 |
| Recording | 3 min / 19 min / 20 min, on `Instant` | — | MeetingSuggestion (once) / DurationWarning / Finish("duration-limit") | 282-312, duration.rs |
| Recording | Cancel | — | Idle; pending cleared; warning cleared | 492-503, loop ~629-638 |
| Processing | PTT press | — | remember | 100-123, 420-460 |
| Processing | PTT release | a press is remembered | forget | 100-123 |
| Processing | toggle press | nothing remembered / one remembered | remember / forget | 100-123 |
| Processing | Cancel | — | stays Processing; remembered press dropped | 492-503 |
| Processing | Finish | — | ignored | 549-555 |
| Processing | ProcessingFinished | press remembered | Idle, then Start{pressed_at=now, latched=remembered} | 505-521 |
| Processing | ProcessingFinished | nothing remembered | Idle | 505-521 |
| any | the coordinator thread panics | — | thread gone; later sends log "channel closed"; Swift is not told | 600-650 |

Out-of-band events:
- Second press during Processing: it is remembered or forgotten silently. No event reaches the UI.
- Cancel during Processing: it only clears the memory. The pipeline keeps running (A2).
- Timeouts: grace 50 ms, tap 400 ms, suggestion 3 min, warning 19 min, cap 20 min. All use `Instant`. [INFERENCE: on macOS `Instant` does not advance during sleep, so the cap counts awake time only.]
- Tap timing uses the moment a command is taken off the channel. `start()` runs on this same thread, so a release queued behind a slow start is stamped late (A11).
- Model switch mid-run: the run plan is frozen at Start and handed to Stop (152-163, 744).
- Setting change mid-run: handler.rs 59-66 reads `push_to_talk` on every event. Flipping it while holding the key leaves the recording open until the next press.
- Late callback: ProcessingFinished fires before the main-thread delivery finishes (A1).

#### M2. Audio recording state
Owner: `managers/audio.rs`, with `audio_toolkit/audio/recorder.rs`.

The state is several parts together:
- RecordingState: Idle, Recording{binding, spool}, or Stopping
- the stream: open or closed
- who holds the capture lease: nobody, dictation, or a meeting
- `capture_generation` (readiness)
- `cancel_generation`
- mute: did_mute and prev_muted

Invariants:
- The spool has exactly one owner at a time: the Recording state, then either the stop task's `SpoolClaim` or the cancel's keep task.
- `is_recording()` is true for Recording or Stopping (1869).
- Mute is applied only while the readiness generation is still current, and this is checked under the mute lock (1105-1146).

| from | event | guard | to | path:line |
|---|---|---|---|---|
| Idle | try_start_recording | stream closed (default: `lazy_stream_close` and `always_on_microphone` both off) | open the stream (about 150 ms, comment at actions.rs 1853), then Recording{spool} plus a readiness thread | 1478-1544; actions.rs 1866-1925 |
| Idle | try_start_recording | stream already open (always-on mic, lazy close within 30 s, or a meeting holds the lease) | Recording almost at once | 1478-1544, 51, ~990-1012 |
| Recording | stop_recording | — | Stopping (spool taken), extra buffer (checks for cancel), `rec.stop()` (the spool is finished before the reply, recorder.rs 2025-2027), then Idle and the stream closes unless held | 1722-1861, 1379-1400 |
| Stopping | stop outcome | cancelled / Err / overrun / VAD none / complete | None+spool / None+spool / Overrun / NoSpeech (short clips padded to 1.25 s) / Complete | 1722-1861 |
| Recording | cancel_recording | — | cancel_gen++, Idle, `rec.stop()`, unmute, close, return the spool | 1880-1921 |
| Stopping or Idle | cancel_recording | — | cancel_gen++ only; returns None | 1880-1921 |
| readiness thread | first samples | generation still current | "recording-ready" (overlay window only), start sound, mute | actions.rs 1883-1925; overlay.rs 762-765 |
| any | stop begins | — | capture_generation++, so a late ready signal or mute is dropped | 1703-1705; actions.rs 1961-1962 |

Out-of-band events:
- Device loss and overrun belong to the audio helper.
- A meeting sharing the stream means no reopen at the next start, which matters for A1.
- A crash mid-recording leaves a spool whose header was never finished. Recovery repairs it.

#### M3. Cancel keys
Owner: `shortcut/mod.rs`; the handler is `shortcut/handler.rs`.

State: `CANCEL_KEYS` is None, Held or HandsFree (62-74). `REGISTERED_CANCEL` holds what is registered now (76-78).

Which keys are live:
- Held: the bare cancel key, plus one `cancel#{prefix}` for each transcription binding that has modifiers. The command binding counts only when command mode is on.
- HandsFree: only the `cancel#` variants. The bare key stays only if the cancel binding itself has a modifier (112-123, 236-262, 270-277).

Invariant: cancel keys are registered only while audio is Recording or Stopping. The handler acts only on a press while `is_recording()` is true (handler.rs 69-74). Every `cancel#` id maps to Cancel (handler.rs 25).

| from | event | to | path:line |
|---|---|---|---|
| None | a recording starts | Held if `push_to_talk`, else HandsFree | 81-89; actions.rs 1932 |
| Held | latch (a tap, or a latched Start) | HandsFree | 92-102; coordinator execute |
| any | stop | None | 105-108; actions.rs 1965 |
| any | cancel | None | utils.rs 78-111 |
| any change | — | a spawn_blocking sync to the newest wanted set; it converges | 125-185 |

Out-of-band events:
- The sync is asynchronous, so for a few milliseconds the old set is still live.
- A binding edited mid-recording takes effect at the next sync.
- `push_to_talk` is read once, at register time.
- Key-only bindings produce no prefix (A5).

#### M4. Stop pipeline and delivery
Owner: `actions.rs`, `TranscribeAction::stop` 1957-2449. Delivery (C1) runs on the core's main thread at 2322-2371.

Stages:
1. S0 stop requested: tray set to Transcribing (1974), streamPhase working (1985), cancel keys off (1965), unmute (~1991), `cancel_generation` captured (~2011)
2. S1 `stop_recording` (2023), with `SpoolClaim` created at 2024-2028
3. S2 decode (2133-2134). It cannot be cancelled.
4. S3 no-speech and held-cloud checks
5. S4 rewrite. It can be cancelled through `complete_unless_cancelled` (615-633).
6. S5 save the row (~2305)
7. S6 queue C1 (2322). The task then ends: `SpoolClaim` drops, then `FinishGuard` sends ProcessingFinished.
8. S7 C1 delivery

Invariants:
- Every branch ends with the tray at Idle.
- The spool's fate is decided before ProcessingFinished (declaration order: 2015, then 2024).
- The row with the words is saved before delivery.
- Delivery is never retried.
- A cancel is honoured up to C1's own check (2323) and ignored after it.

| from | outcome / event | guard | to | path:line |
|---|---|---|---|---|
| S1 | NoSpeech | cancelled since gen | Idle, nothing saved | 2030-2036 |
| S1 | NoSpeech | — | no-speech row (rolled back if cancelled during the save), Idle | 2037-2051, 1489-1500 |
| S1 | Complete | cancelled | Idle | 2066-2071 |
| S1 | Complete | VAD silent, cloud-only | no-speech row, Idle | 2079-2098 |
| S1 | Complete | no samples | Idle, no row, no message | 2101-2107 |
| S1 | Complete | — | WAV written alongside (`sona-{stop second}-{run}.wav`, 1300-1302), decode starts | 2108-2134 |
| S2 | Err | cancelled / not cancelled | Idle / failed row + "transcription-error", Idle | 2386-2421 |
| S2 | Ok | cancelled | Idle | 2160-2165 |
| S3 | HeldCloudUnavailable / no speech confirmed | — | row, Idle | 2196-2198 / 2219-2244 |
| S4 | rewrite | cancel arrives | Idle | 2246-2264 |
| S4 | after rewrite | cancelled | Idle | 2266-2271 |
| S5 | final text empty | — | row + not_dispatched, Idle | ~2290-2300 |
| S5 | text | — | row saved; on failure only a log line (1239-1268); C1 queued | ~2305, 2322 |
| S1 | Overrun | cancelled / not | Idle / truncated prefix row, Idle | 2424-2441 |
| S1 | None | — | Idle, no row, no message | 2442-2448 |
| S7 | C1 starts | `was_cancelled_since(gen)` (global) | not_dispatched, Idle | 2323-2335 |
| S7 | C1 | — | `deliver` (blocking: 100 ms chord, paste_tx 183; +50 ms auto-submit, delivery/mod.rs 267-275; legacy 60+60 ms, clipboard.rs 995/1018; external script up to 10 s, 612/703), delivery event, paste-error, receipt, **Idle** | 2337-2370 |
| S6 | scheduling fails | — | not_dispatched, Idle | 2372-2382 |

Out-of-band events:
- Cancel at each stage:
  - S1: honoured at once
  - S2: waits for the decode to finish
  - S4: honoured at once
  - S7: checked once, then ignored
- A crash or quit during S2 to S5: A3, A7.
- A busy main thread delays C1.
- A new recording started at the drain while C1 is still running: A1.

#### M5. Spool and crash recovery
Owners: `actions.rs` (`SpoolClaim` 1414-1450, `keep_cancelled_capture` 1454-1487) and `managers/history.rs` (`spool_path` 1132-1138, `keep_spool` 1406-1452, `recover_interrupted_dictations` 1456-1521). Startup is in `lib.rs` 2373-2397.

A spool file moves through these states:
1. Writing: in the in-progress folder, header not yet finished
2. Finished
3. Then one of: Removed; Kept as Cancelled (row + notice); Left (for the next launch)
4. At the next launch, a Left spool becomes one of: Removed; Kept as Interrupted (row + "didn't finish" notice); Left again

Invariants:
- J1: a spool never sits beside a row that holds its run receipt.
- J2: every file in recordings is referenced by a row.

| from | event | guard | to | path:line |
|---|---|---|---|---|
| — | start | — | Writing (`sona-{start second}-{run}.wav`) | actions.rs 1861-1866; recorder.rs 875-899 |
| Writing | cancel_recording | under 10 s / 10 s or more | Removed / Kept as Cancelled + "dictation-kept" | 1408, 1454-1487 |
| Writing | stop | — | Finished (`spool.finish` before the reply) | recorder.rs 2025-2027 |
| Finished | SpoolClaim drop | receipt exists / cancelled since gen / limit 0 / otherwise (a `has_run_receipt` error counts as no receipt) | Removed / Kept as Cancelled / Removed / Left | 1414-1450 |
| Left or crash orphan | launch | listed when HistoryManager is built; only the in-progress folder is read | queued | history.rs 811, 3253-3265 |
| queued | recovery after unlock, once per launch | repair error / under 1 s / `for_retry` error / save error / limit 0 / ok | Left / Removed / all Left / renamed back / file removed / row + id in `recovered_this_launch` | 1456-1521, 1437-1444; lib.rs 2376-2377 |
| Kept row | 14 days | sweep at startup, insert, or limit change | deleted | 404-409, 2129-2148 |

Out-of-band events:
- A crash during the decode leaves a stray WAV, breaking J2 (A3).
- Locked storage: A7.
- A crash between the save and the claim breaks J1 (A7).
- The headless query never recovers spools (lib.rs 2221-2245).

#### M6. Retention
Owners: `managers/history.rs`, `settings.rs`, `commands/history.rs`.

Row classes:
- saved (always kept)
- unsaved
- kept: a Cancelled or Interrupted receipt, no words, has audio
- derived: has a `parent_id`

Invariants:
- Kept rows are outside the count sweep.
- At limit 0 there are no new rows.
- A derived row's file exists as long as the row does (A10 breaks this).
- A WAV that two rows share survives until both are gone (2007-2037).

| from | event | guard | to | path:line |
|---|---|---|---|---|
| — | insert (`save_entry_with_receipt_internal`) | limit 0 | a non-derived file is removed and no row is written; a derived row gets Ok(None) | 1540+ |
| — | insert | otherwise | row, then `cleanup_old_entries` | 1540+, 1916-1957 |
| cleanup | count sweep | unsaved, not kept, past the limit by timestamp (newest first) | rows and their files deleted | 2046-2071, 2007-2037 |
| cleanup | kept sweep / recording expiry | 14 days / retention period | deleted | 2129-2148 / 2080-2125 |
| — | `update_history_limit(n)` | — | the setting changes and cleanup runs at once | commands/history.rs 351-366 |
| — | startup | — | cleanup, once | lib.rs ~2392-2396 |
| settings load | schema below 20 and limit is 5 | — | limit becomes 100 | settings.rs 2834-2842 |
| parent | deleted | — | a trigger nulls the children's `parent_id` (no foreign key) | 199-202 |
| kept parent | a retry child with words is saved | — | no longer kept, so it falls under the count sweep | 404-409 |

#### M7. Swift capture and pill mirror
Owner: `macos/Sona/AppModel.swift`.

State:
- `capture`: idle, recording, or working(kind) (235)
- `pillFeedback`
- `meetingSuggested` (265)
- `meetingAfterCapture` (268)
- `pillWarningSeconds`
- `notice`
- `coreStopped`

Invariant: `capture` equals the core's stage only if events arrive in stage order. A1 and A2 break this.

| from | event | to | path:line |
|---|---|---|---|
| any | activity "recording" | .recording; notice cleared; meter on | 1083-1097, 683-699 |
| any | activity "transcribing" / streamPhase working(kind) | .working | 1093-1094 / 1104-1108 |
| any | other activity (idle) | .idle; feedback dismissed after a delay; meeting suggestion cleared; `recordMeeting()` if `meetingAfterCapture` | 1095-1096, 683-699, 464 |
| any | core closed | coreStopped; .idle | 858-863 |
| .recording | meeting suggestion | shown; dropped in any other state | 1196-1197 |
| .recording | delivered feedback / kept feedback | dropped / shown | 710-720, 1186-1195 |
| user | `toggleCapture` | .idle starts via `hud_toggle_recording`; .recording stops; .working does nothing | 639-645 |
| user | Cancel (shown in recording and working) / "Finish and start a meeting" | `cancel_operation` / stop + `meetingAfterCapture` | 660-663, 667-677; Panels.swift 46-59, 108; CaptureScreen.swift 152-166 |

`syncPill` (956-985) hides the pill and the duration warning whenever `capture` is idle and there is no feedback. Nothing else sets `.recording`: "recording-ready" goes only to the overlay (overlay.rs 762-765), and the mic level only feeds the meter (1103).

#### M8. Swift Library mirror
Owners: `macos/Sona/LibraryStore.swift` and `LibraryScreen.swift`.

State:
- `phase`
- `limit`, which starts at 0 (100)
- `retention`, `savingRetention`
- `retrying` set, `deleting` set
- `kept`, plus the announced-once flag
- the field's `limitText` (starts "", 1055) and `editingLimit`

Invariant: the limit on screen equals the core's limit. Before the first successful load this does not hold (A4).

| from | event | to | path:line |
|---|---|---|---|
| any | Library appears | `start()`: six requests in parallel, on every appearance | LibraryScreen 46; Store 147-155 |
| any | `loadSettings` succeeds / fails | limit = `historyLimit ?? 100` / limit unchanged plus an error | 631-639 |
| any | core restart | `coreStarted`, which reloads only the kept list | 258-262; AppModel 851 |
| any | history, kept, storage, settings events | applyHistory + loadKept / loadKept / reload (not settings) / loadSettings | 123-143 |
| field | appears / limit changes / submit or focus lost | text = limit / text = limit only if not focused / `commit`, then `updateLimit` | LibraryScreen 1089, 1090-1092, 1093-1107 |
| store | `updateLimit(n)` | only if `next != limit`; request; then limit = next | 647-661 |
| row | Transcribe / Transcribe again | guarded per row; button disabled while busy | 467-479; LibraryScreen 303-326, 561-562, 703-705 |
| sheet | Process | the sheet's `busy` disables Process and Cancel; the store has no guard | 1180-1247; Store 489-491 |

#### M9. Library retry and reprocess
Owner: `commands/history.rs` 230-347.

Flow:
1. Freeze a plan: `RunPlan::for_retry` or `for_reprocess` (313-347)
2. Read the whole WAV (237-245)
3. One decode over the whole file, holding the FIFO engine lease, with no cancel (249-256; transcription.rs ~2376-2381)
4. Empty result → error (~258-260)
5. Rewrite (~262)
6. Save the derived row; limit 0 → error (~264-291)
7. Delivery receipt `not_dispatched` (~292-297)

Invariants: it never pastes, and each success makes one derived row.

Out-of-band events:
- Live dictation needs the engine meanwhile (A8).
- A count sweep runs during the retry (A10).
- History is off (A9).
- The core dies mid-retry: Swift clears `retrying` and nothing is saved.

## Transcription

### After the audit fixes

#### M2. Engine slot and lease (replaces the map's M2)
Fields: `engine: Mutex<EngineSlot>` (T:654-694) and `engine_lease_gate`. `current_model_id`, `active_engine_lease` and `reload_model_on_next_use` are gone.

States the types allow:
- Empty.
- Resident(build, engine): `build` is an `EngineBuild` (T:633-649): model id, transcribe accelerator, GPU device, ONNX accelerator.
- Leased(build): a batch or stream owner holds the engine and the gate, and the model reads as loaded. The map's E2, E3 and E6 collapse into this state; E6 (stale id after a worker panic) is Leased with the engine gone, healed by the next batch decode's reload.

| from | event | guard | to | path |
|---|---|---|---|---|
| Empty / Resident(b) | load for plan p | gate held | Empty (old engine dropped first), then Resident(for_plan(p)), or Empty with `loading_failed` | T:1504, T:1665-1673 |
| Resident(b) / Leased(b) | `initiate_model_load(p)` | for_plan(p) == b | no load | T:1705-1707 |
| Empty / Resident(b'), b' ≠ for_plan(p) | `initiate_model_load(p)` | no load in flight | loader thread, then the load row | T:1700-1719 |
| Resident(b) | batch decode for p | gate; `serves(for_plan(p))` | Leased(b), then Resident(b) via `put_back` | T:2418-2419, T:2448, T:2740 |
| Resident(b') / Leased(b') / Empty | batch decode for p | gate; not `serves` | load for p, then the row above | T:2419-2422 |
| Leased(b) | engine panic in a batch decode | — | Empty, `unloaded` with an error | T:2749 |
| Resident(b) | stream worker reaches the engine | gate; `claim.serve()`; `lease(for_plan(asr))` | Leased(b), then Resident(b) on every exit | T:1959, T:1969, T:2017, T:2180, T:2185 |
| Resident(b') / Empty | stream worker | lease refused | unchanged; the worker drains and the stop decodes in batch | T:1969-1975 |
| Resident | manual, idle or immediate unload | gate | Empty, `unloaded` | T:1067-1090 |
| not Leased | `put_back` | — | unchanged; the returned engine is dropped | T:685-692 |

Reads: `is_model_loaded` = a build is present (T:1272); `get_current_model` = that build's model id (T:1722-1726); `current_backend` = a Resident engine only (T:1783-1791).

M3 note: the idle watcher's `model_resident` = a build is present (T:1121), so during a batch decode the watcher sees a resident, busy engine instead of NotResident, and the unload deadline counts from the end of the decode.

Accelerator setters (shortcut/mod.rs:1590-1622): save the setting only. The next use of any kind compares the plan's build with the slot's and reloads on a mismatch.

#### M4 and M5. Stream route claim (adds to the map's M4 and M5)
| from | event | guard | to | path |
|---|---|---|---|---|
| — | `StreamRouter::open` | — | QUEUED, one `WorkerClaim` shared by the route and the lanes | T:344-355 |
| QUEUED | worker holds the gate | CAS QUEUED → SERVING | SERVING; the worker leases the engine | T:194-203, T:1958-1962 |
| QUEUED | `finalize_stream` | CAS QUEUED → ABANDONED | ABANDONED; finalize returns `Ok(None)`, and the stop decodes in batch | T:207-216, T:2282-2284 |
| ABANDONED | worker holds the gate | CAS fails | the worker returns without the engine and releases the gate | T:1959-1962 |
| SERVING | `finalize_stream` | CAS fails | Finalize sent; waits up to 30 s for the reply, as before | T:2285 onward |

The map's out-of-band case ("finalize can arrive while the worker is still in W0") now ends in ABANDONED at once; the 30 s wait applies only to a worker that holds the engine.

#### M6. Batch decode (steps that changed)
4. Load when the slot does not serve the plan's whole build (T:2418-2422).
5. Lease the engine; the slot keeps the build (T:2448).
6. whisper: prompted when `model_is_whisper && prompt_vocabulary && !without_prompt && speech != Undetected`, with the prompt built to fit whisper's budget (T:2509-2521); span recovery for every prompted decode (T:2558-2568).
9. `model_produced_text` (T:2795-2799) also reads as nothing a whisper decode under Undetected that is only a stock silence phrase.
10. Post-process: exact correction for the prompted entries and fuzzy for the rest only when `vocabulary_prompted_from` gives a first entry (the audio fits the first 30 s chunk and nothing was decoded again); fuzzy for everything otherwise (T:3778-3786, T:3908 onward).

#### M7. Span recovery redo rule
| condition | decoded again without the prompt | path |
|---|---|---|
| Detected | every lost stretch: the whole window when nothing was heard, else every uncovered stretch of 5 s or more | T:3024-3025 |
| Unknown or Undetected, some segment heard words | only the lost stretches that hold a segment echoing the prompt | T:3030-3035, T:3012-3018 |
| Unknown or Undetected, nothing heard | none: output that is only the prompt means nothing was said | T:3036 |

The map's first unchecked invariant ("whisper saw the whole `vocabulary_initial_prompt`") now holds by construction (S:1144-1168, T:3769-3773).

#### M8. Speech evidence
| condition | evidence | path |
|---|---|---|
| VAD on and it forwarded speech | Detected | A:2154 |
| VAD on, nothing forwarded, capture of 15 s or less | raw clip decoded as Undetected: no vocabulary prompt, and a stock silence phrase counts as nothing | A:2155, T:2512, T:2795-2799 |
| VAD on, nothing forwarded, capture over 15 s | no-speech receipt, no decode | audio.rs:1828 (unchanged) |
| VAD off | Unknown | A:2156 |
| Library retry, headless check | Detected | H:253, lib.rs:1347 |
| import, meeting, chat | Unknown | T:2362 |

#### M9. Dictation stop and outcome
| outcome | what happens | path |
|---|---|---|
| Transcribed, text | delivered on the main thread, after the focus check | A:2447-2475, A:1354-1393 |
| Transcribed, emptied by cleanup | row saved as not dispatched, no notice (filler-only or "scratch that", by design) | A:2237 |
| DecodedNothing (Detected, the model wrote nothing) | `transcription-error`, failed row with the WAV, not-dispatched receipt | A:2272-2280, A:471-490 |
| NoSpeech (Undetected or Unknown, the model wrote nothing) | no-speech receipt and a `recording-error` notice | A:2244-2271 |
| Err | `transcription-error`, failed row with the WAV | A:2444, A:471-490 |
| text ready, focus moved since stop | `DefinitelyNotDispatched`, `paste-error` with `focus_moved`, text in the Library and held for "Paste here", no submit | D:151-166, A:1366-1376 |
| held, "Paste here" clicked | the held words go to the control focused now, never submitted; a new dictation or another refusal drops them | A:1397-1403, A:1338-1349, A:1996 |

#### M10. Delivery and the reliable paste transaction
Route: focus check (D:151-160), then accessibility insertion, then the clipboard (C:901-960).

| from | event | guard | to | path |
|---|---|---|---|---|
| text ready | dispatch | the method inserts at the focus, and the pid or element read at stop and now differ | DefinitelyNotDispatched; nothing inserted or submitted; the words go back to the caller | D:151-166, D:239-245, D:118-124 |
| text ready | dispatch | same focus, a part unreadable, or method None or ExternalScript | accessibility insertion, then the clipboard, as before | D:182 onward |

Transaction states (P), changed steps:
1. Snapshot: every item, every type, its bytes (P:269, C:974-994).
5. Settle: the owed Enter after a read; restore the whole snapshot when `changeCount` still matches (P:174, C:997-1016).
Legacy path: snapshot at C:1051, restored at C:1093 when the clipboard is still ours.

#### M11. Model commands
| from | event | guard | to | path |
|---|---|---|---|---|
| selection A | "Use" B | `engine_busy()` (recording, stream worker, import, batch decode, load), read before the switch claims its own load scope | B saved, `selection_changed`, nothing loaded; the next run loads B | M:105-106, M:133-152 |
| selection A | "Use" B | idle, unload "Immediately" | same as above | M:133-152 |
| selection A | "Use" B | idle, any other timeout | load B; revert the selection on failure | M:154-160 |

### As mapped

#### M1. Engine load scope (`EngineTransition`)
Owner: `src-tauri/src/managers/transcription.rs`. The pieces are `try_begin_load` (:614), `wait_for_load_idle` (:639), `LoadingGuard` drop (:663), `UnloadingGuard` drop (:687), and the nested `begin_load` inside the loader (:1376-1378).

- **States:** Idle (depth 0); Loading (depth ≥1, nested loads add depth); Unloading flag (set only inside `unload_engine` while the gate is held).
- **Unnamed combination:** Loading while the loader thread is blocked on the engine gate behind someone else's long decode. The scope is held but nothing is loading.
- **Invariants:** the scope is held from the decision to load until the load ends (`initiate_model_load` moves the guard into its thread, :1636-1643). Every batch and stream owner waits for depth 0 before taking the gate (`wait_for_load_then_lock_engine_lease` :1192-1201).

| from | event | guard | to | path |
|---|---|---|---|---|
| Idle | `initiate_model_load(plan)` | `try_begin_load` ok | Loading (thread); immediately Idle again if `!reload_pending && id==plan && is_model_loaded()` | :1626-1655, check :1631-1635 |
| Loading | `initiate_model_load` | depth>0 | no-op | :1627-1629 |
| Idle | `switch_active_model` | scope free | Loading (blocks the caller on the gate) | commands/models.rs:100-102, :145 |
| Loading | `switch_active_model` | depth>0 | Err "Model load already in progress" | commands/models.rs:100-102 |
| Loading | guard drop | — | Idle; notify `load_idle` (not the idle watcher) | :663-685 |

Out-of-band: a queued loader (Loading + gate wait) makes every new batch or stream owner wait (:1194), makes every model switch fail, and marks the engine busy (`engine_busy` about :1690-1700), so meeting background passes skip their turn.

#### M2. Engine slot and lease
Fields: `engine: Mutex<Option<LoadedEngine>>`, `current_model_id`, `active_engine_lease`, `engine_lease_gate`.

States the types allow:
- **E0 Empty:** engine None, id None, lease 0.
- **E1 Resident:** engine Some, id m, lease 0.
- **E2 Batch-leased:** engine taken (:2404-2418), id m, lease 0, gate held by the batch until `drop(engine_lease)` (:2746). Nobody named this state. `is_model_loaded()` (:1177-1181) and `current_backend()` (:1715) report "not loaded" here.
- **E3 Stream-leased:** engine taken, id m, lease = worker id, gate held by the worker (:1891, CAS :1907-1917, take :1918).
- **E4 Stream-draining:** engine back in the slot, lease = worker id, gate still held. Reached after the stream fails to begin (:2022-2025 → :2131-2139), and lasts until finalize or cancel.
- **E5 Reloading:** gate held. `_while_leased` drops the engine and id first (about :1452-1460), then ends in E1, or in E0 with `loading_failed`.
- **E6 Stale id:** id m, engine None, lease 0. Reached when the stream worker panics (its feed loop has no `catch_unwind`). Harmless, because `model_is_ready` checks both fields (:2384-2389).
- **E7 Mismatched return:** `return_engine` drops the engine if the id changed mid-lease (:2222-2235).

| from | event | guard | to | path |
|---|---|---|---|---|
| E0/E1 | load (any) | gate | E5 → E1/E0 | :1348-1364 (gate :1356), :1366+ |
| E1 | batch decode | gate | E2 → E1 | :2382, :2404-2418, :2746 |
| E2 | engine panic | — | E0, `unloaded` event with error | about :2700-2740 |
| E1 | stream worker start | gate + lease CAS | E3 | :1891-1935 |
| E3 | begin fails | — | E4 until finalize | :2022-2025, :2131-2139 |
| E3/E4 | finalize/cancel | — | E1 (guard drop clears lease) | :2140-2146, :755-770 |
| E1 | `unload_model` / idle unload / immediate unload | gate | E0 | :1251-1265, :1033-1058, :1282-1302 |
| E2/E3 | unload or switch | — | waits on the gate | :1252, :1356 |

#### M3. Idle watcher
Owner: `IdleWatcherContext::run` (:1060-1134), `idle_wait` (:900-937), `automatic_unload_blocked` (:939-955).

- **States:**
  - NotResident: condvar wait with no deadline. This is also the state during every E2, because `model_resident()` (:1017-1020) is false there.
  - Indefinite: timeout is Never, or Immediately while blocked.
  - Deadline(wait).
  - Busy(wait): resets `last_activity`.
  - Unload: rechecks under the gate (:1033-1058).
- **Blocked means:** recording, a load in flight, a stream worker, a media import, or batch uses above the allowance. The watcher's allowance is 0; `maybe_unload_immediately` uses 1.
- **What wakes it:** guard drops (:715, :732, :755), `start_stream` (:1795-1796), and the touch plus signal after a load. `set_model_unload_timeout` does not wake it (commands/transcription.rs:21-27).

#### M4. StreamRouter
Owner: transcription.rs:273-418. Fields: `route`, `open`, `accepting_audio`, `preview_degraded`, `first_speech_armed`, `first_speech_start`.

- **States:**
  - R0 Idle.
  - R1 Armed: one-shot start waiting, no route.
  - R2 Open: route, accepting.
  - R3 Degraded: route, not accepting, degraded.
  - R4 Taken: no route. A worker may still be alive; the only protection is `start_stream` refusing while `active_stream_worker != 0` (:1781).

| from | event | guard | to | path |
|---|---|---|---|---|
| R0 | dictation start, streaming model | — | R1 | actions.rs:1823-1829 → :1743-1750 → :319 |
| R1 | VAD-forwarded frame | one-shot present (lock held across start) | R2 via `start_stream` → `open` | :363-376, :1780-1803, :299 |
| R2 | lane full (2 s), disconnected, decoder error | — | R3 | :413-418; worker :2062-2064 |
| R1/R2/R3 | `finalize_stream` / `cancel_stream` | — | R4 | :2248 / about :2292 |
| R2 | worker fallback (model mismatch, lease taken, no engine, not streamable or several languages) | — | R0 via `clear`; the drain exits at once | :1893-1902, :1907-1917, :1918-1935, :1972-1976 |
| R2 | stream begin fails | — | stays R2, no `clear` | :2131-2139 |

`open()` resets `preview_degraded`, which is shared across dictations. That is safe only because of the guard at :1781.

#### M5. Stream worker
Owner: `run_stream_worker` (:1879-2147); finalize contract in `finalize_stream` (:2247-2289).

- **States:**
  - W0 waiting for load idle and then the gate (:1891). Nothing reads the lanes; after 2 s of queued audio the preview is degraded.
  - W1 fallback drain, exits at once.
  - W2 streaming (:2036).
  - W3 degraded: feeds are dropped.
  - W4 finalizing: replies Some, or None if degraded or `finalize()` failed.
  - W5 begin-failed drain: holds the gate.
  - W6 cancelled.
  - W7 exit: `return_engine`, then reply (:2142-2144), then guard drop.
- **What finalize returns:**
  - Ok(None): batch fallback.
  - Ok(Some) with model text: the stream text is used.
  - Ok(Some) without text: batch (actions.rs:482-495).
  - Err after the 30 s timeout (:2262-2268): the dictation errors. There is no batch fallback, by doc comment (:2240-2243).
- **Out-of-band:** finalize can arrive while the worker is still in W0. The worker can also outlive its finalize: it then processes the stale Finalize, its reply is dropped, and until it exits it blocks new streams and automatic unload.

#### M6. Batch decode (`transcribe_shared_with`, :2348-2856)
Steps in order:
1. Count the batch use (:2367).
2. Empty audio: return early (:2374-2377).
3. Take the gate (:2382).
4. Load if `model_is_ready` is false (:2384-2389). This ignores the reload flag.
5. Take the engine (:2404-2418).
6. Decode, per engine:
   - whisper: prompted only when `model_is_whisper && prompt_vocabulary && !without_prompt` (:2480-2484); windows (:2515-2543); SpanRecovery only when speech is Detected (:2521-2531, filter :2523).
   - parakeet: one `transcribe_with` call over the whole capture (:2584-2593).
   - moonshine: 64 s and 30 s windows.
7. On panic: E0.
8. Release the gate (:2746).
9. Echo filter on the whole joined text, unless anything was recovered (:2755-2757). The vocabulary-only filter runs for unprompted plans only (:2758-2759). `model_produced_text` is set at :2760-2761.
10. Post-process: exact correction if prompted and nothing was recovered, fuzzy otherwise (:2791; branch :3832-3836).
11. Force "" when the model produced nothing (:2834-2838).
12. `maybe_unload_immediately` (:2848).

#### M7. Whisper window and SpanRecovery
Owner: `audio_chunks.rs` and transcription.rs:2864-3066.

Per window:
1. Cut: `window_end` (audio_chunks.rs:37-64) needs a pause of at least 100 ms at RMS ≤0.01 within the last 2 s, otherwise it cuts hard at the ceiling.
2. Decode with the prompt. On InputTooLong or OutputTruncated, halve the window, up to 3 levels.
3. `heard_spans` judges each segment with `model_heard_text` (:2959-2966, :3682-3686).
4. `lost_spans`: the whole window when nothing was heard, otherwise every uncovered stretch of at least 5 s (:2915, :3049-3066).
5. Decode each lost span again without the prompt (:2968-3008).
6. `splice_recovered` drops every segment whose midpoint lies in a lost span, even when the redo came back empty (:3012-3037).

`recovered_words` flips false→true once and stays true for the whole capture (:2579-2581).

Two invariants the code assumes but never checks:
- whisper saw the whole `vocabulary_initial_prompt`.
- an echo starts at the prompt's first word (`repeats_decode_prompt` :3688-3697).

#### M8. Speech evidence
| condition | evidence | path |
|---|---|---|
| VAD on and it forwarded speech | Detected | actions.rs:2128-2132 |
| VAD on, nothing forwarded, capture ≤15 s | raw clip decoded, Unknown | audio.rs `model_arbitrates_vad_silence`, about :1797 |
| VAD on, nothing forwarded, capture >15 s | no-speech receipt, no decode | |
| VAD off | forwarded = true, Unknown | recorder.rs about :1949, :2063 |
| Library retry, headless check | Detected | history.rs:253, lib.rs:1347 |
| import, meeting, chat | Unknown | `transcribe_shared` :2328-2330 |

#### M9. Dictation stop and outcome
Owner: actions.rs:2014-2452. Every branch after stop calls finalize or `cancel_stream` (:1936, 2032, 2037, 2067, 2081, 2104, 2431, 2440, 2448).

| outcome | what happens | path |
|---|---|---|
| NoSpeech | receipt and a `recording-error` notice | |
| Err | `transcription-error`, failed row with the WAV | :2396-2399 |
| Transcribed, empty | row saved as not dispatched; no event, no notice | :2293-2302 |
| Transcribed, text | delivered on the main thread | :2322 |

Cancel is checked at :2262-2271 and :2273-2278, and again inside delivery (:2323). Nothing checks focus.

#### M10. Delivery and the reliable paste transaction
Route: accessibility insertion first (context/macos.rs:500-548), then the clipboard (clipboard.rs:962-1035).

Transaction states (paste_tx/macos.rs):
1. Snapshot: plain text, else one image (:271-279).
2. Publish the promise (about :294-299).
3. Send the chord. On failure it sets `injection_failed` and still returns Ok (:307-320).
4. Wait for a read followed by 200 ms of quiet; or 8 s with no read; or 500 ms after a failed chord; or loss of ownership (`spawn_waiter` :203-256).
5. Settle exactly once (:126-189): the owed Enter after a read; restore text, else image, else clear, only if `changeCount` still matches.

`PENDING` holds one transaction; `flush_pending` settles it before the next paste (:193-201). Every clipboard dispatch maps to DispatchedButUnconfirmed (delivery/mod.rs:149-151, 165-173), which the Swift pill shows as "Sent".

#### M11. Model commands and the Swift mirror
- `switch_active_model` (commands/models.rs:94-154): claim the scope; write the selection before loading (:111-122); under Immediately emit `selection_changed` and load nothing (:124-142); otherwise load and revert the selection on failure (:145-151).
- `delete_model` (commands/models.rs:66-88): unloads only if the model is selected. It has no check for an in-flight frozen plan.
- Swift `modelOperations` (AppModel.swift:1137-1153): `loading_started` → loading, `loading_failed` → failed, `unloaded` with an error → notice, anything else ends the phase.
- The bridge runs one task per request (native_bridge/mod.rs:305-330).
- The core starts with `--no-tray` (Core.swift:206), so the Rust tray is never built in the Mac app.

## Meeting recording

### After the audit fixes

These replace M1 through M7 in `MapMeetingRecording.md`; paths are on `audit/meeting-capture`.

#### M1. Session phase (persisted)
Owner: `meeting/store.rs` holds the phase column, revision, events and allowed actions (`allowed_actions` 8469, narrowed by `session_snapshot` 4290-4298). `meeting/session.rs` drives the transitions.

**States:** Preflight, Starting, CapturingRecording, CapturingPausing, CapturingPaused, CapturingResuming, Stopping, Processing, ReviewReady, RecoveryRequired, Deleting, and gone (row deleted).

**Allowed actions (store 8469; Paused narrowed at 4290-4298):**

| Phase | Actions |
|---|---|
| Preflight | Refresh, CancelPreflight, Start |
| Starting, CapturingRecording, CapturingPausing, CapturingResuming | Pause, Stop, Discard |
| CapturingPaused | Resume (only while some lane has not Failed), Stop, Discard, Edit |
| Stopping, Processing | Discard |
| ReviewReady | Edit, Regenerate, Export, Delete |
| RecoveryRequired | FinalizePartial, Discard, Delete |
| Deleting | none |

**Gating:** unchanged. User commands check the revision first, then the phase; system transitions check the phase only. A refusal comes back as `Ok(Rejected receipt)`.

**Invariants:**
- Capturing* means `actor.active == Some(this)` holding one `CaptureHold` (session.rs 548-565): the lease, the processing park, and keep-awake (released only while Paused). Only that capture's own drop, pause or resume changes the hold.
- Starting and Stopping are transient on every path: a failed start returns to Preflight, a dead writer no longer blocks Stop. A store error while sealing still leaves Stopping, with nothing held; Discard works and the next launch recovers it.
- RecoveryRequired is only entered by the mount sweep, which runs once per launch before any command sees the store.
- ReviewReady gets `delete_after` when the retention policy is finite (store 3934-3956).

**Transitions:**

| From | Event | Guard | To | Path |
|---|---|---|---|---|
| none | gate opens, tray preflight, or detection `open_capture` | — | Preflight; `tray_session_id` set | session 1084-1179; detection 2557-2620 |
| Preflight | Start (gate, panel, standing series) | revision; `CaptureLease` acquired (1726) | Starting; then one `CaptureHold` (keep-awake on, processing parked, lease) | store 3754; session 1760-1773 |
| Starting | every lane registered, window open, `capture_started` written | — | CapturingRecording; `active = Some(this, hold)`; `tray_session_id` cleared | session 1776-1798; `begin_capture` 1855-1987 |
| Starting | no lane started, or a required one missing | — | Preflight "source_start_incomplete"; lanes aborted, writers stopped; hold dropped | session 1800-1808, 1821-1830 |
| Starting | any error after lanes opened (track, writer, window, transition) | — | Preflight "start_failed"; window closed; every opened lane aborted, writers stopped; hold dropped; error returned with the row re-emitted | session 1800-1840; `begin_capture` 1940-1944 |
| CapturingRecording | Pause | revision, phase | Pausing → Paused (window closed and phase written before devices pause); devices paused, a refusing lane noted Failed; keep-awake off | session 2094-2124; `confirm_pause` 4729 |
| CapturingPausing | pause write fails | — | CapturingRecording "pause_failed", window reopened, devices untouched; error returned | session 2108-2117; `undo_pause` 4749 |
| CapturingPaused | Resume | revision, phase (the UI offers Resume only while some lane has not Failed; the store does not refuse a stale press) | Resuming (keep-awake on) → Recording with a new window if any lane resumed; else Paused "resume_failed", keep-awake off; a lane that cannot resume is aborted and noted Failed | session 2150-2215; `confirm_resume` 4769 |
| CapturingResuming | resume write fails | — | CapturingPaused "resume_failed"; resumed lanes paused again (aborted if they refuse); keep-awake off; error returned | session 2185-2206; `undo_resume` 4802 |
| Starting or Capturing* | Stop (UI, panel, detection, recording card) | revision, phase | Stopping; takes `active` only if it is this meeting (`take_if`, 2273); stops lanes; a dead writer is logged and its lane marked Failed | session 2234-2298 |
| Stopping | lanes sealed | — | Processing; `tray_session_id` set; hold dropped with the capture; job submitted | session 2302-2339 |
| Stopping | store error while sealing | — | stays Stopping, nothing held (Discard allowed) | session 2302-2328 |
| Processing | job done | — | ReviewReady; `delete_after` set | store 3934-3956 |
| any live phase an earlier launch left | first store mount of the launch | — | RecoveryRequired, plus an event recording the prior phase | session 4286-4287; store 6823-6880 |
| Preflight an earlier launch left | same sweep | — | gone | store 6825, 6907 |
| Starting import an earlier launch left | same sweep | — | gone, as a Discard | store 6826, 6931-6957 |
| RecoveryRequired | Finish (recovery band or auto pass) | revision, phase | Processing; its run waits behind a live capture's park | session 2893-2918 |
| any but Deleting | Delete, Discard, retention, or a failed import | — | Deleting → gone. Tombstone always queued. Another meeting's capture and hold untouched; this meeting's own capture taken, lanes aborted, writer errors ignored. Discard keeps no restore bundle. | session 2771-2831; store 6300-6440 |
| Deleting | first store mount of the launch | — | `resume_deletions` | store 6791, 6824 |

**Out-of-band events:**
- quit or crash: M9, C4
- sleep: M7 (a paused meeting is not stopped by it)
- two Stops: the second is refused
- the retention sweeper tick: removes only its own meeting's capture, if any
- a recovery-band action during a capture: its run waits behind the park
- an import running in parallel: M12
- a command before the startup call: it mounts and sweeps first

#### M2. Session actor (in memory)
Owner: session.rs 518-600. Fields: `active: Option<ActiveCapture{sources, live, call_names, audibility, hold: CaptureHold}>` and `tray_session_id`.

**Combinations the types allow:** a Stopping row with `active == None` after a store error while sealing (nothing is held then). A command that names another meeting cannot take `active`.

**Transitions:**

| From | Event | To |
|---|---|---|
| None | start succeeds | Some(A) with its hold (1776-1791) |
| None | start fails at any step | None; opened lanes aborted, writers stopped, hold dropped (1800-1830) |
| Some(A) | stop(A) | None (`take_if` 2273); lanes stopped; the hold drops after the actor lock (2334) |
| Some(A) | stop(B) | Some(A) unchanged; NotFound |
| Some(A) | delete(A) | None; lanes aborted; writer errors ignored (2813-2829) |
| Some(A) | delete(B), discard(B), retention(B) or finalize(B) | Some(A) unchanged, hold intact |

**`tray_session_id`:**
- Set at 1177 and at 2329.
- Cleared at 1791, by `cancel_preflight` (1253), by `tray_snapshot` when its row is gone (1055), and by delete (2810-2811).
- `tray_snapshot` (1035) returns `active` first.

#### M3. Track worker (one thread per lane)
Owner: session.rs 588-591, `TrackWorker::start` 4547, `stop` 4565, `run_track_worker` 4575.

**States:**
- Running: a 5 ms polling loop.
- Exited(Err): any `?` on `record_clock_epoch`, `record_gap` or `accept_with_bridge`. Still nothing watches; the next Stop or Discard finds it.
- StopRequested → Sealing → Joined(Result): `stop` returns the thread's Result. Stop logs an Err and marks the lane Failed (2282-2297); delete ignores it (2824-2829).
- Leaked: no path drops a handle without `stop` any more. A failed start stops every writer it started (1800-1803).

**Invariant:** each live source has a Running worker, or its lane is marked Failed at Stop.

#### M4. Keep-awake
Owner: `CaptureHold.keep_awake` (session.rs 549), one per capture; `meeting/keep_awake.rs` releases on drop.
- Acquired: at start, with the hold (557-558); on Resume (2162).
- Released: Pause confirmed (2124); Resume with no lane resumed (2207-2209) or unwritten (2202); with the hold when the capture ends (stop 2334, delete 2824-2830, failed start 1830).
- Invariant: held from the Starting commit until the capture ends, except while Paused (a stop from Paused keeps it off). No other meeting's command can reach it.

#### M5. Processing capture park
Owner: `meeting/processing.rs`. `capture_active: AtomicBool` (884), written only by `park_for_capture` (1178-1184, true) and `CaptureGate::drop` (907-911, false). `wait_for_capture` (3236) waits while it is true.
- Parked by the hold at start (session.rs 561).
- Lifted only when the hold drops: stop 2334, delete of the capturing meeting 2824-2830, failed start 1830.
- Invariant: true exactly while a capture is live; one owner.

#### M6. Capture lease
Owner: session.rs 524-543, acquired at 1726, held inside `CaptureHold` (552) from the Starting commit.
- Read by `is_capture_active` (4243), cloud-sync deferral (cloud_sync/runtime.rs 1521, 1623, 2686), note_shares and `utils::capture_running`. The tombstone guard is gone.
- The park (M5) and the lease drop together with the hold, park first (550-552), so the two cannot disagree while a capture is live.

#### M7. Detection tracking
Owner: `meeting/detection.rs` and `detection/machine.rs` (unchanged).

**States:**
- Untracked
- Tracked(Detection{session, trigger_bundle_id, event_end, sources}); a detection-opened gate is tracked while Preflight or Starting
- Tracked(Operator(Waiting))
- Tracked(Operator(Adopted{bundle}))
- RuntimeState also holds `slept` (`begin_tracked` 742-746 and `end_tracked` 751-756 reset it; a paused phase clears it, 816-818)

**Transitions:**

| From | Event | To | Path |
|---|---|---|---|
| Untracked | panel Record, or a standing-series auto-start | Detection with event_end | 2439-2468 |
| Untracked | notification accepted → `open_capture` | Detection, keyed to the gate | 2557-2620 (2610-2611) |
| Detection gate | tick while the gate is Preflight or Starting | unchanged | 819-825 |
| Detection gate (same session) | Start from the gate | Detection over the started lanes, trigger app and event end kept (only when detection is enabled) | 2470-2476; 763-786 |
| other or Untracked | Start from the gate | Operator(Waiting) (only when detection is enabled) | 763-786 (777) |
| Waiting | a call becomes active | Adopted (no event end; mic lane false) | 616-636 |
| Tracked | tick while Pausing, Paused or Resuming | `slept` cleared; the other stop rules still apply | 816-818 |
| Tracked | tick: tray session gone, another session, or a non-capture phase | Untracked; `track_ended` hides the panel (Detection) and finishes the recording card | 826, 2627-2656; 2507-2513, 2532-2547 |
| Tracked | tick: a stop trigger fires | spawn `stop(expected_revision)`. Committed → `track_stopped` ends tracking, then "auto-record stopped" is recorded. Rejected or Failed → stays tracked, nothing recorded; the next tick decides again | 2688-2714; 793-802, 2517-2528 |
| Tracked | recording card Stop or Forget | Committed → Untracked; refused → stays tracked, card stays | 2352-2402 (2396) |
| Tracked | operator `meeting_stop` Committed | Untracked | commands/meeting.rs (unchanged) |

**Stop rules, in order** (machine.rs 954-985, unchanged): SleepBoundary, EventEnd (no grace), TriggerAppExited, CallEnded (hang-up grace), InputDeviceIdle. A paused phase clears `slept` first, so SleepBoundary only fires for a sleep while recording.

The tick is every 15 s, or sooner on an input-device edge. It reads the tray session (1279-1282): capture phases count as live, and a detection gate in Preflight or Starting stays tracked.

#### M10. Screen recorder core (recorder.rs, screen_recorder.swift)

**Phases (recorder.rs 18-30):** unchanged.

| From | Event | To | Path |
|---|---|---|---|
| Idle | preflight | Checking → Idle; reply `session: None` | recorder.rs:242-287 |
| SelectingSource, Previewing, Starting, Recording, Paused, Finalizing | preflight | phase unchanged, no emit; reply `session` = the held snapshot (`held_session`); start availability still reports the recorder's own lease as CaptureBusy, which the app ignores when `session` is set | recorder.rs:256-286, 208-210 |
| Saved, Failed, Permission | preflight | unchanged; `session: None` | recorder.rs:267-276 |
| Idle, Saved, Failed or Permission | preview_start | unchanged from the map | recorder.rs:289-367 |
| Previewing | preview_stop | Idle | recorder.rs:369-389 |
| Previewing | start | Starting → native start → re-check → Recording | recorder.rs:391-442 |
| Recording | pause | Paused; a pending native failure finalizes first (row below) and pause then answers InvalidState | recorder.rs:444-475 |
| Paused | resume | Recording; same pending-failure rule | recorder.rs:477-505 |
| Recording or Paused | stop | Finalizing → native stop (writer finished) → rename → Saved, `failure` = any pending native failure as a notice. Finalize error → `native_cancel` → `fail(notice or error)` → Failed, partial removed | recorder.rs:507-592 |
| Recording or Paused | native failure (heartbeat, or any command's `apply_native_failure`) | `finalize` with the failure as notice: Saved with `failure` set when the writer can still finish; Failed with the partial removed when it cannot | recorder.rs:661-670 → 522-592 |
| any other phase holding native | native failure | `fail` → Failed or Permission; partial removed | recorder.rs:671-674, 677-704 |
| any phase where `cancel_reaches` holds | cancel | Idle; partial removed | recorder.rs:594-613, 753-766 |
| — | manager built at launch | every `.sona-screen-*.partial.mp4` in the recordings folder removed before the manager exists | recorder.rs:228-229, 835-848 |

**Native side:** `report()` (screen_recorder.swift:1054-1076) records the first non-drop status as `failure` and stops the inputs with the writer kept (`stopInputs(cancelWriter: false)`); later statuses only update `failure`. `stopSynchronously` (352-371) finishes the writer; `finishWriting` (897-901) refuses a writer not in `.writing`, which lands on Failed through the Rust finalize error path.

**Invariants:**
- Saved means the final file exists. `failure` on a Saved snapshot is a notice that the recording ended early, not a loss.
- A partial on disk belongs to a recording in flight in this process; launch removes any other.
- Preflight names any capture the recorder holds, so a reopened sheet can always reach it.

#### M11. Swift recorder mirror (RecorderStore.swift, RecorderTypes.swift, RecorderSheet.swift)

| From | Event | To | Path |
|---|---|---|---|
| any | `.preflight` with `session` | the held snapshot: the sheet shows its controls (Paused: Resume, Stop & save, Discard) instead of setup or CaptureBusy | RecorderTypes.swift:305-307 |
| any | `.preflight` without `session` | unsupported → failure; CaptureBusy → failure; otherwise setup (unchanged) | RecorderTypes.swift:308-314 |
| Saved | snapshot with `failure` | Saved view plus a notice naming the failure | RecorderSheet.swift:157-159 |
| Recording or Paused | quit → Stop and Quit | `stopAndSave()` task awaited by the delegate → Finalizing → Saved | RecorderStore.swift `stopAndSave`; SonaApp.swift `applicationShouldTerminate` |
| Starting or Finalizing | quit | quit waits (`RecorderPhase.activity == .settling`) until Recording, which it then asks about, or Saved, which it does not | RecorderTypes.swift `activity`; AppModel.swift `waitForCapturesToSettle` |
| Starting, Recording, Paused or Finalizing | menu bar | a line names the recording and its clock; "Record the Screen" becomes "Stop Screen Recording", disabled while Starting or Finalizing | SonaApp.swift `ScreenRecordingStatus`, `ScreenRecordingMenuItem` |

#### M9. App lifecycle, quit (SonaApp.swift)

| From | Event | To | Path |
|---|---|---|---|
| nothing recording, starting or finishing | Quit (Cmd+Q, menu bar, AppleScript, Dock, Privacy, logout) | notes and scratchpad flush → terminate (unchanged) | SonaApp.swift `applicationShouldTerminate` |
| a sheet is up | Quit by AppleScript or the Dock (`kAEQuitApplication`) | Sona's own handler, not AppKit's refusal: the sheet closes, then terminate, as Quit Sona does | SonaApp.swift `applicationWillFinishLaunching`, `quitEvent`; AppModel.swift `quit` |
| dictation transcribing, recorder Starting or Finalizing, or a meeting start request out | Quit | waits until it settles, then the rows below | AppModel.swift `waitForCapturesToSettle` |
| meeting Starting or Stopping in the core | Quit | the active-state read takes the meeting actor lock the start or stop holds, so it answers once the meeting records (asked) or is sealed (not asked) | session.rs `tray_snapshot`, `start_with_provenance`, `stop` |
| a meeting in Capturing*, recorder Recording or Paused, and/or dictation Recording | Quit | alert naming what records | SonaApp.swift `confirmStopping` |
| alert | Keep Recording (Escape) | quit cancelled; sheet, meeting and recording untouched | SonaApp.swift `applicationShouldTerminate` |
| alert | Stop and Quit | dictation finish sent → flush → `meeting_stop` surface `quit` → recorder stop awaited → dictation awaited until idle (words delivered and saved) → terminate | SonaApp.swift `applicationShouldTerminate`; AppModel.swift `waitForDictationToFinish` |
| Stop and Quit | `meeting_stop` refused and the meeting still captures | Sona stays open: "The meeting could not be stopped, so it is still recording." | SonaApp.swift:137-142, 157-175 |
| next launch | meeting stopped by quit | RecoveryRequired with prior phase Stopping or Processing → automatic reprocessing when local, model present and no audio missing; else Unfinished meetings | existing recovery (session.rs `reprocess_recovered`) |

#### Tray presence (lib.rs, tray.rs)

| Launch | `show_tray_icon` | Tray | `set_tray_visibility` | Path |
|---|---|---|---|---|
| `--no-tray` (the Mac app) | any | Absent: never built | no-op | lib.rs:771-775, 1055-1070; tray.rs:964-973 |
| no flag | true | Shown | shows or hides | lib.rs:778-999 |
| no flag | false | Hidden: built, then hidden | shows or hides | lib.rs:989-991 |

#### Tray meeting menu (tray.rs, lib.rs; tray builds only)

| Meeting phase | Store actions (store.rs 8418-8452) | Cancel Meeting Notes | Click does | Path |
|---|---|---|---|---|
| Preflight | CancelPreflight | shown | `cancel_preflight` | tray.rs:84-107; lib.rs:918-955 |
| Starting, Capturing* | Discard | shown | `discard`, one click (unchanged) | same |
| Stopping, Processing | Discard | hidden | nothing; the handler re-checks the fresh snapshot | same |
| RecoveryRequired | FinalizePartial, Discard, Delete | hidden | nothing | same |
| ReviewReady, Deleting | no Discard | hidden | nothing | same |

### As mapped

#### M1. Session phase (persisted)
Owner: `meeting/store.rs` holds the phase column, revision, events and allowed actions (8418-8455). `meeting/session.rs` drives the transitions.

**States:** Preflight, Starting, CapturingRecording, CapturingPausing, CapturingPaused, CapturingResuming, Stopping, Processing, ReviewReady, RecoveryRequired, Deleting, and gone (row deleted).

**Allowed actions (store 8418-8455):**

| Phase | Actions |
|---|---|
| Preflight | Refresh, CancelPreflight, Start |
| Starting, CapturingRecording, CapturingPausing, CapturingResuming | Pause, Stop, Discard |
| CapturingPaused | Resume, Stop, Discard, Edit |
| Stopping, Processing | Discard |
| ReviewReady | Edit, Regenerate, Export, Delete |
| RecoveryRequired | FinalizePartial, Discard, Delete |
| Deleting | none |

**Gating:**
- User commands check the revision first, then the phase. System transitions check the phase only.
- A refusal comes back as `Ok(Rejected receipt)`, not as an `Err`.

**Intended invariants and how they break:**
- Capturing* means: `actor.active == Some(this)`, the capture lease is held, the processing gate is true, and keep-awake is held (except while Paused).
  - C1 and C11 break the gate and keep-awake while recording.
  - C10 and C6 leave the gate and keep-awake held with no capture.
- Starting and Stopping are meant to be transient. C10 strands a session in Starting and C6 strands one in Stopping, until the next launch or a Discard.
- RecoveryRequired is only entered by the startup sweep.
- ReviewReady gets `delete_after` when the retention policy is finite (store 3929-3934; types 711-714).

**Transitions:**

| From | Event | Guard | To | Path |
|---|---|---|---|---|
| none | gate opens, tray preflight, or detection `open_capture` | — | Preflight | session 1138-1162; detection 2355-2425 |
| Preflight | Start (gate, panel, standing series) | revision; `CaptureLease` acquired at 1710 | Starting; keep-awake on (1758); gate true (1759) | store 3749, 3844; session 1745-1759 |
| Starting | no source started, or a required one missing | — | Preflight "source_start_incomplete"; gate false (1880); keep-awake off (1881) | session 1855-1896 |
| Starting | sources, workers and window all open | — | CapturingRecording; `tray_session_id` cleared | 1904-1929 |
| Starting | any `?` error between 1823 and 1916 | — | stays Starting (C10) | 1823-1916 |
| CapturingRecording | Pause | revision, phase | Pausing → Paused; keep-awake off (2096) | 2045-2096 |
| CapturingPausing | `?` error | — | stays Pausing (C18) | 2057-2094 |
| CapturingPaused | Resume | revision, phase | Resuming (keep-awake on, 2126) → Recording if any source resumed; else Paused "resume_failed" with keep-awake off (2171) and failed sources aborted (2151) | 2117-2210 |
| Starting or Capturing* | Stop (UI, panel, detection, recording card) | revision, phase | Stopping; takes `active` (2269-2273); stops workers (2275-2280) | 2232-2280 |
| Stopping | tracks sealed | — | Processing; gate false (2312); keep-awake off (2313); `tray_session_id` set (2314) | 2297-2314 |
| Stopping | `?` error | — | stays Stopping (C6) | 2275-2311 |
| Processing | job done | — | ReviewReady; `delete_after` set | store 3929-3934 |
| any live phase | startup sweep | — | RecoveryRequired, plus an event recording the prior phase | store 6809-6870 |
| Preflight | startup sweep | — | gone | store 6890-6905 |
| RecoveryRequired | Finish (recovery band or auto pass) | revision, phase | Processing; gate false (2899), whatever the other sessions are doing | session 2880-2909 |
| any but Deleting | Delete, Discard, retention, or a failed import | — | Deleting → gone. Keep-awake off and gate false, unconditionally (2792-2794). Tombstone only if nothing is capturing (2780-2790). Discard keeps no restore bundle (store 6340-6345). | session 2780-2817; store 6286-6420 |
| Deleting | startup | — | `resume_deletions` | store 6810 |

**Out-of-band events:**
- quit or crash: M9, C4
- sleep: M7, C14
- two Stops: the second is refused
- the retention sweeper tick: C1
- a recovery-band action during a capture: C1, C11
- an import running in parallel: M12
- an interleaved startup sweep: C16

#### M2. Session actor (in memory)
Owner: session.rs 518-568. Fields: `active: Option<ActiveCapture{sources, live worker, call_names, audibility, _capture_lease}>` and `tray_session_id`.

**Combinations the types allow:**
- a Starting or Stopping row with `active == None` (C10, C6);
- `active == Some(B)` while a command targets A (latent L1).

**Transitions:**

| From | Event | To |
|---|---|---|
| None | start succeeds | Some(A) (~1904-1929) |
| None | start fails with `?` | None; sources dropped and aborted; TrackWorkers leaked (C10) |
| Some(A) | stop(A) | None, taken at 2269-2273 |
| Some(A) | delete(A) | None; workers stopped in a loop whose `?` can stop early (2812-2817) |
| Some(A) | delete(B) or finalize(B) | Some(A) unchanged, but M4 and M5 are cleared (C1, C11) |

**`tray_session_id`:**
- Set at 1138-1162 (also while a capture is active, which is harmless) and at 2314.
- Cleared at 1929, by cancel_preflight, and by delete (2797-2800).
- `tray_snapshot` (1022-1044) returns `active` first.

#### M3. Track worker (one thread per lane)
Owner: session.rs 565-568, `start` 4507-4521, `stop` 4523-4530, `run_track_worker` 4533-4612.

**States:**
- Running: a 5 ms polling loop.
- Exited(Err): any `?` on `record_clock_epoch`, `record_gap` or `accept_with_bridge` (store 7354-7466). Nothing watches for this.
- StopRequested → Sealing → Joined(Result): `stop` returns the thread's Result.
- Leaked: the handle is dropped without `stop`. There is no Drop impl, so the thread polls until the process exits.

**Invariant:** each live source has a Running worker. This is violated silently (C6).

#### M4. Keep-awake
Owner: `meeting/keep_awake.rs`. One idempotent IOKit assertion pair, not reference-counted.
- Acquired at 1758 and 2126.
- Released at 1881, 2096, 2171, 2313 and 2794. The release at 2794 runs for whichever session is being deleted.
- Invariant: held exactly when a capture is in Starting, Recording or Resuming. C1 breaks it one way; C6 and C10 break it the other.

#### M5. Processing capture gate
Owner: `meeting/processing.rs`.
- `capture_active: bool`. `wait_for_capture` (3219) has no timeout and is waited on at 1815, 1871, 2125 and 2287.
- Set true at session 1759. Set false at 1880, 2312, 2793 and 2899.
- `live_pass` skips while the engine is busy (3112-3114). So while a processing job runs, the live transcript stops updating.
- Invariant: true exactly when a capture is live. Several sessions write it and none owns it.

#### M6. Capture lease
Owner: session.rs 514, 523-542, acquired at 1710 and held as `ActiveCapture._capture_lease`.
- Read by `is_capture_active` (4232), cloud-sync deferral (cloud_sync/runtime.rs 1521, 1623), note_shares 13, the tombstone guard (2780-2790) and `utils::capture_running` (116-124).
- It is a different flag from M5, and the two can disagree:
  - C1 and C11: M6 true, M5 false.
  - C10: M6 false, M5 true.

#### M7. Detection tracking
Owner: `meeting/detection.rs` and `detection/machine.rs`.

**States:**
- Untracked
- Tracked(Detection{session, trigger_bundle_id, event_end, sources, slept})
- Tracked(Operator(Waiting))
- Tracked(Operator(Adopted{bundle})) (RuntimeState 487; `begin_tracked` 634-638 resets `slept`; `end_tracked` 643-650)

**Transitions:**

| From | Event | To | Path |
|---|---|---|---|
| Untracked | panel Record, or a standing-series auto-start | Detection with event_end | 1933-1944 (event_end at 1941); 1235-1250 |
| Untracked | notification accepted → `open_capture` | Detection, keyed to the preflight snapshot | 2355-2425 (2412-2418) |
| any | Record from the gate | Operator(Waiting), only when detection is enabled | 2292-2304 |
| Waiting | a call becomes active | Adopted (no event end; mic lane false) | 652-711 (690-699) |
| Tracked | tick: `active` is None or another session | Untracked; `track_ended` hides the panel (Detection) and finishes the recording card | 2431-2440; 2334-2348 |
| Tracked | tick: a stop trigger fires | spawn `stop(expected_revision)`. Any `Ok(_)`, Rejected included, records "auto-record stopped" (idempotent key, workflow_engine 341-361) and ends tracking | 2462-2489 |
| Tracked | recording card Stop or Forget | `Ok(_)` → Untracked | 2166-2219 (2207-2213) |
| Tracked | operator `meeting_stop` Committed | Untracked | commands/meeting.rs 387-406 |

**Stop rules, in order** (machine.rs 954-985), with no phase input:
1. SleepBoundary
2. EventEnd, with no grace period
3. TriggerAppExited
4. CallEnded, with the hang-up grace period
5. InputDeviceIdle (mic lane, Sona not holding the device, and the device not running)

The tick is every 15 s (85), or sooner on an input-device edge. `active_capture` (2499-2514) keeps Capturing* phases, CapturingPaused included.

**Out-of-band events:** sleep sets `slept`; a stale revision (C8); a second tick during an in-flight stop (refused, and the record key is idempotent, so no harm); detection being turned off mid-capture (ticks continue while tracking).

#### M8. Swift live mirror
Owner: MeetingLiveStore.swift.

**Fields:** `gate`, `live`, `active` (the "Recording now" card, fed by `consent_panel_active_state` at session 1581-1612 for Capturing* only; Shell.swift 311-327), `pending`, `error`, `recovery`, `starting`.

**Behavior:**
- Not optimistic. Every action goes through `pending`. `receive` (1807-1820) sets `error` on a refusal.
- `sessionChanged` (553-567) refreshes the session and `loadActive` after `eventSettle`.
- `capture()` (883-918) calls `recover(from:)` (1823-1840) on a throw.
- The Record guard requires `.preflight` (867-868).
- Stop paths: `stopFromPanel` 811-840, `stop` 1131-1149, `discard` 1152-1170.
- Recovery: `finalizeRecovery` 1479-1494, `discardRecovery` 1497-1512. The recovery band (MeetingStartView 543-590) is disabled only while `pending` is set.

**Combinations the types allow:**
- a gate on a Starting session, where Record does nothing (C10);
- the recovery band enabled during a live capture (C1, C11).

#### M9. App lifecycle
**Quit:**
- `applicationShouldTerminate` (SonaApp 60-87) only flushes notes and the scratchpad.
- A cancelled quit sets `model.sheet = nil` (94).
- `applicationWillTerminate` (108-111) calls `core.shutdown()` (Core 223-245), which waits 3 s and then terminates.
- The core handles "shutdown" with `app.exit(0)` (native_bridge/mod.rs 343-346).
- `RunEvent::Exit` (lib.rs 2436-2446) shuts down only cloud sync, the agent panel and the model.
- `watch_parent` (native_bridge 225-234) exits the core if its parent dies.
- The menu bar's "Quit Sona" (SonaApp 203) has no recording guard.

**Startup:**
- The recovery task (lib.rs 699-714) runs alongside the socket bind (2308-2327). An Err is logged once and never retried.
- `recover_at_startup_at` (session 675-687) sets `recovery_complete`. Only `cloud_store` reads it (4209-4212).
- The sweep (store 6809-6870) runs `resume_deletions`, then discards preflights, then resolves status. It then SELECTs the live phases, Starting included, and for each runs `repair_session_tracks` (7206-7245, a `?` per session) and moves it to RecoveryRequired.
- Automatic reprocessing runs only when the prior phase was Stopping or Processing (`reprocess_withheld` 847-886). It runs one meeting at a time (745-813).
- Retention sweeps every 15 min (69, 662-672 → `sweep_retention_at` 917-1001), and once more at startup.

#### M10. Screen recorder core
Owner: `recorder.rs`, plus `screen_recorder.swift`.

**Phases (18-30):** Checking, Permission, Idle, SelectingSource, Previewing, Starting, Recording, Paused, Finalizing, Saved, Failed.

**Transitions:**

| From | Event | To | Path |
|---|---|---|---|
| Idle | preflight | Checking → Idle. In other phases the phase is left unchanged, no snapshot is emitted, and any active capture lease reports CaptureBusy | 215-258 |
| Idle, Saved, Failed or Permission | `preview_start` | SelectingSource, picker runs unlocked, mic lease taken after the picker (else `fail(CaptureBusy)`), phase re-checked, then Previewing; picker cancelled → Idle | 260-338 |
| Previewing | `preview_stop` | Idle | 340-360 |
| Previewing | `start` | Starting → native start → re-check → Recording | 362-412 |
| Recording | pause | Paused, or `fail` | 415-446 |
| Paused | resume | Recording, or `fail` | 448-476 |
| Recording or Paused | `stop` | Finalizing → native stop unlocked → rename → Saved. No phase re-check afterwards. On error: `native_cancel`, then `fail` | 478-553 (errors 512-527) |
| any phase where `cancel_reaches` holds (Finalizing included) | cancel | Idle; partial removed | 555-574; 732-743 |
| any | `fail` | Failed or Permission; partial removed | 632-659 |

**Heartbeat:** every 1 s (595-621). It calls `apply_native_failure` (622-631) and emits a snapshot only while Recording.

**Native side:** `report()` (1055-1076) sends any non-drop status to `cancelSynchronously` (374-377). That runs `stopInputs(cancelWriter: true)` (848-883; `cancelWriting` at 880) and `removePartialOutput` (947-951).

**Fatal reports:**
- `didStopWithError` (558-566)
- an append failure (653-655)
- a start failure (456-458)
- an invalid, negative or non-monotonic timestamp (757-790)

The writer is not fragmented. The partial file is named `.sona-screen-{id}.partial.mp4` (793-797), and nothing sweeps these files (history.rs 3253-3257 lists only `.wav` files in the dictation spool folder).

**Invariants:**
- the native recorder exists from Previewing through Finalizing;
- the mic lease is held exactly when the native recorder is running with the mic;
- Saved means the final file exists.

#### M11. Swift recorder mirror
Owner: RecorderStore.swift, RecorderTypes.swift, RecorderSheet.swift.
- The reducer is at RecorderTypes 300-354. `.preflight` shows CaptureBusy as a failure.
- `run()` dispatches the target phase optimistically and falls back to `.streamFailed` on an error.
- `start()` (114-122) resets the state and reloads the preflight.
- `stop()` (127-132), called on disappear, discards only a preview.
- The sheet is not dismissible by the user while it can't close (RecorderSheet 32). `.task`/`.onDisappear` are at 43-44.
- Only Recording and Paused offer the discard confirm. Finalizing shows no buttons.
- There is no command to read the snapshot, so the mirror only learns the phase from replies and the Recording-only heartbeat.

#### M12. Import
Owner: session.rs 2344-2721.
- `import_recording_with_notes` (2355-2401) → `open_imported_session` (Starting, no lease) → `write_imported_audio` (2620-2636, a long blocking decode while in Starting) → `seal_imported_capture` (2638-2693: Recording → Stopping → Processing, no flags touched) → submit.
- Any error → `abandon_import` (2697-2721) → `delete_with_cause(Discard)`, which clears M4 and M5.
- Phone recordings: the cloud head is written only after the import succeeds (cloud_sync/runtime.rs 3356-3398).
- The local import queue lives only in memory (media_import.rs).

## Meeting notes and ledger

### After the audit fixes

M1. Processing job registry (processing.rs)
| from | event | guard | to | path |
|---|---|---|---|---|
| no entry | `submit` (stop, import, recovery) | none | registered, Notes stage | processing.rs:1388-1418 (1403) |
| registered, Notes | run's status written and review entered | entry is this run's (`ptr_eq`) | registered, People stage | 1701-1705, 1530-1540 |
| no entry | `begin_rewrite` | none | registered, Notes stage (rewrite) | 1502-1526; session.rs:3634-3637 |
| registered, Notes | `begin_rewrite` | stage is Notes | refused, NotesInProgress; nothing runs | 1507-1510 |
| registered, People | `begin_rewrite` | stage is People | old flag set (its People pass stops at the next person); entry replaced by the rewrite's | 1511-1520 |
| registered (any) | `cancel` from delete | entry exists | flag set; reaches a rewrite too | 1347-1356; session.rs:2841 |
| registered (run) | run returns | `ptr_eq` | no entry | 1409, 1421-1433 |
| registered (rewrite) | receipt written, or failure, or panic (job dropped) | `ptr_eq` | no entry | 1018; session.rs:3644-3654, 3680 |
| no entry | `start_relationship_refresh` | no entry | registered, People | 1453-1496 (1473) |
| registered | `start_relationship_refresh` | entry exists | refused | 1453-1496 |
| registered | each request's checkpoint | flag set → Cancelled; relay no longer allowed → RemoteUnavailable | run stops | 2887-2908 |

M2. ProcessingStatus
| from | event | guard | to | path |
|---|---|---|---|---|
| Running (relay run) | next request's checkpoint after cloud AI off or series kept here | `route_still_allowed` false | Failed{RemoteUnavailable} | processing.rs:1327-1336, 2902-2904, 1685-1691 |
| any | rewrite fails | none | Failed (unchanged rule, D13) | session.rs:3655-3669 |
| any | rewrite receipt committed | revision unchanged since the press (a rewrite no longer moves it itself) | Succeeded, same transaction | store.rs:6237-6276; processing.rs:2862 |
| any | second rewrite while one writes notes | Notes-stage job | unchanged, NotesInProgress | session.rs:3634-3637 |
| Pending or Running, phase Processing, at launch | recovery sweep | notes for the current transcript exist | Succeeded | store.rs:6844-6846, 6898-6909 |
| Pending or Running, at launch | recovery sweep | no such notes | Failed{Interrupted} (unchanged) | store.rs:6848-6878 |

M3. MeetingPhase, notes slice
| from | event | guard | to | path |
|---|---|---|---|---|
| Processing | run ends | `allowed_from = [Processing]` | ReviewReady or RecoveryRequired; then all follow-ons (`FollowOns::All`) | processing.rs:1719-1773 |
| Processing, at launch | recovery sweep | notes for the current transcript exist | ReviewReady (event `processing_finished`); follow-ons once after pending workflows resume: All, or InsideSona when the reprocess would be withheld | store.rs:6898-6923; session.rs:770-791, 4362 |
| Processing, at launch | recovery sweep | no such notes | RecoveryRequired (unchanged) | store.rs:6848-6878 |
| ReviewReady | rewrite commits | `first_notes` (no other revision has content) | ReviewReady; inside-Sona follow-ons once (workflow event, wrap card, folder prompts) | store.rs:6257-6263; session.rs:3687-3693; processing.rs:1777-1828; automations.rs:204-227 |
| ReviewReady | rewrite commits | not first notes | ReviewReady, no follow-ons | session.rs:3687 |

M4. Notes revisions
| from | event | guard | to | path |
|---|---|---|---|---|
| (none) | pipeline stores notes | intent Pipeline | Current; title derived after (unchanged) | processing.rs:2835-2868 |
| (none) | rewrite stores notes | intent Rewrite | Current; title derived in the receipt transaction, after the revision check | processing.rs:2862; store.rs:6277-6283 |
| (none) | plain notes after a refusal | `refuses_structured_reading(plain_because)` | Current, plain, no ledger, `ledger_failure` = the refusal | processing.rs:2815-2818, 5948 |
| (none) | plain notes after a time limit or oversized pack | other cause | Current, plain, ledger read as for structured notes | 2819-2832 |
| Current | second rewrite while one writes | Notes-stage job | no second row | session.rs:3634-3637 |

M6. Plain notes fallback
| from | event | guard | to | path |
|---|---|---|---|---|
| notes reading failed (not Stopped, not Unreachable) | plain read succeeds | none | plain content with `plain_because` = the notes' cause, no ledger yet | processing.rs:5915-5946 |
| plain content | ledger gate | cause is SafetyBlocked, ModelDeclined, ReplyNotStructured or ReplyRejected | ledger skipped, `ledger_failure` = cause | 2815-2818, 5948 |
| plain content | ledger gate | any other cause | ledger generated; failure, if any, is the ledger's own | 2819-2832 |
| plain revision on screen | band text | `plainBecause` present, else `ledgerFailure` (older rows) | PlainNotesBand reason | MeetingNotesPane.swift:21; MeetingTypes.swift:1043-1046 |

M7. Ledger merge (complete_reading.rs)
| from | event | guard | to | path |
|---|---|---|---|---|
| ≥2 parts read | merge reply parses with a headline | none | `apply_merge` result | 1413-1418 |
| ≥2 parts read | merge stopped | Stopped | ledger fails, run stops | 1419 |
| ≥2 parts read | merge fails any other way | not Stopped | every part kept, first part's headline, PARTS_NOT_JOINED caveat first | 1420-1424, 10 |
| open loop listed as answered | `apply_merge` | answering thread settled and later; an Unanswered thread cites the loop's first citation | asked thread takes the answer's state; loop and answering thread removed | 1325-1376 |
| open loop listed as answered | `apply_merge` | no Unanswered thread cites the question | loop kept | 1346-1357 |

M8. Engine choice
| from | event | guard | to | path |
|---|---|---|---|---|
| run or rewrite starts | engine chosen | relay when cloud AI on, relay reachable, series not kept here; else local; else none | `ChosenEngine { generator, route }` | processing.rs:907, 1153 |
| relay route | each request | cloud AI off or series kept on this Mac | stop, RemoteUnavailable | 1327-1336, 2902-2904 |
| relay route | each person in the People pass | same | pass ends | 2941-2946 |
| local route | each request | none | continues (never rechecked) | 1333 |

M9. Swift mirrors
| from | event | guard | to | path |
|---|---|---|---|---|
| no action running | an action starts | none | entry appended; `pending` = its word; busy | MeetingsStore.swift:2078-2080, 313-316 |
| rewrite running | rename, note or list action starts and ends | none | only that action's entry removed; busy stays true; rewrite buttons stay disabled | 2080-2082 |
| rewrite running in the core | another press (any window or path) | core Notes-stage job | error "Sona is still writing this meeting's notes." | MeetingTypes.swift:690, 722 |
| meetings home on screen | every 30 s | the view's task is alive | engine re-read; warning follows it | MeetingLiveStore.swift:504-514; Shell.swift:348 |
| meetings home leaves the screen | task cancelled | none | no more re-reads | Shell.swift:348 |

#### M4. Notes revisions

States: Current with content, OutOfDate with content, Failed with no content. A Current row with no content is rejected as Invalid (store.rs:5657).

| from | event | to | path |
|---|---|---|---|
| (none) | the pipeline or a rewrite stores notes | Current; OutOfDate if the session revision moved since the input was read | store.rs:5653-5715 (5674-5679) |
| (none) | same key exists and is not Failed | returned unchanged (Cached) | processing.rs:2550-2562 |
| Failed | same key stored again | replaced | store.rs:5653-5715 |
| (none) | notes and plain notes both fail | Failed row | processing.rs ~2600-2625 |
| Current | any segment, note or speaker edit, title, call-name rename, cross-meeting correction, voice identity, relisten | OutOfDate (every row); content kept | store.rs:9047; callers store.rs:4853, 4967, 5011, 5044, 5079, 5131, 5152, 5361, 5630; call_speakers.rs:334; transcript_corrections.rs:989; voice_identity.rs:895, 1028; relisten.rs:195; trigger transcript_correction_dependencies.sql:22-29 |
| OutOfDate | (none) | never returns to Current | (none) |

Readable revision (new, one rule for every core reader):

| revisions present | readable revision | ledger readers get | path |
|---|---|---|---|
| any with a ledger, at least one current | newest current with a ledger | its ledger | store.rs:8103-8121 |
| any with a ledger, none current | newest out-of-date with a ledger | its ledger (stale reading kept after an edit) | same |
| newer current without a ledger over an older one with one (D3) | the older one with a ledger | its ledger, the one the tab shows | same |
| no ledger anywhere | newest current notes, else newest out-of-date notes | no ledger, no loops | same |
| only Failed rows, or none | none | no ledger, no loops | same |

| reader | uses | path |
|---|---|---|
| list row headline, summary, outcome chips | READABLE_ARTIFACT_CONTENT | store.rs:4447-4450 |
| one meeting's loops, resolve, reopen, assign | `seeds_in` → `readable_ledger_in` | loops.rs:680, 692 |
| corpus walk: inbox, digest, today, external plane | `ledger_rows_in` | loops.rs:403 |
| search index key and text, headline lookup | READABLE_ARTIFACT_ID / _CONTENT | query_plane.rs:66, 76, 227, 309 |
| person page, people list, outcome totals | READABLE_ARTIFACT_CONTENT | people/queries/artifacts.rs:29, 76, 280; people/queries.rs:272 |
| series "last time" brief | READABLE_ARTIFACT_CONTENT | series.rs:391 |
| recall, reel, continuity | `readable_ledger_in` | recall.rs:361; reel.rs:57, 149; people/continuity.rs:271 |
| ledger export | `MeetingStore::readable_ledger` | session.rs:3746; loops.rs:113 |
| Mac ledger tab | `readableLedger`, same order | MeetingTypes.swift:1202-1208 |
| Mac Follow up / Save ledger on the review page | `currentLedger` (current only, unchanged) | MeetingsStore.swift:1976; MeetingTypes.swift:1184 |

Invariant: the core's loop words, the stored loop states and the Mac ledger tab all come from one revision. An edit never moves that revision; only a newer stored revision with a ledger does.

### As mapped

#### M1. Processing job registry
Owner: `src-tauri/src/meeting/processing.rs` (the `jobs` map, one cancel flag per meeting).

States for one meeting:
- no entry
- pipeline registered, flag clear
- pipeline registered, flag set (cancelling)
- relationship refresh registered
- states nobody named: a rewrite running with no entry; a rewrite running while the pipeline's People stage still holds the entry

Invariants:
- At most one entry per meeting.
- A run removes only its own entry. `retire_job` checks `Arc::ptr_eq` (1237-1250).
- A checkpoint reads whatever flag is in the map now, not the run's own flag (`generation_checkpoint` 2694-2709). It checks cancel and transcript presence only. It does not check consent or settings.

| from | event | guard | to | path |
|---|---|---|---|---|
| no entry | submit (Stop, Import, Recovery) | none; the phase already moved to Processing | registered | processing.rs:1210-1235; callers session.rs:2318, 2405, 2472, 2900 |
| registered | `cancel` | entry exists | cancelling | 1176-1182; the only caller is `delete_with_cause` session.rs:2791, after `reserve_deletion` |
| registered or cancelling | run returns | `ptr_eq` | no entry | 1237-1250 |
| no entry | `start_relationship_refresh` | no entry | registered (refresh) | 1269-1310 |
| registered | `start_relationship_refresh` | entry exists | refused, no refresh | 1269-1310 |
| any | `artifacts_regenerate` | none | runs unregistered | 1320-1376; session.rs:3565-3640 |

Out-of-band events:
- Delete during a rewrite: `cancel` finds nothing. The rewrite stops only at its next checkpoint, once the transcript rows are gone. The call already in flight runs to its deadline: 240 s local, 660 s relay.
- Delete during the People stage: `cancel` reaches the pipeline's flag.
- Quit or crash: the map is lost. Launch recovery takes over (store.rs:6808-6873).
- Re-finalizing right after an incomplete recovery run: the new entry replaces the old one. The old run's `retire_job` is a no-op, and its `finish_progress` publishes `progress: None` for the meeting. That only blanks the progress line.

#### M2. ProcessingStatus
Owner: the store column. Writers are in processing.rs, session.rs and store.rs.

States: Pending, Running, Succeeded, Cancelled, Failed{reason, cause}.

Combinations the types allow:
- Running while the phase is ReviewReady or Processing after the run ended. This happens when the status write at 1533 fails: `finish_review` is skipped and the meeting waits for the next launch.
- Failed while good current notes exist (D1, D13).

Invariant: the header describes the newest notes attempt. That was the intent of the Succeeded write inside `record_artifact_regeneration` (store.rs ~6226). D1 breaks it.

| from | event | to | path |
|---|---|---|---|
| Pending | run starts (best effort) | Running | processing.rs:1481-1486 |
| Running | `process` returns `Ok(None)` | Succeeded | 1517-1524 |
| Running | `Ok(Some(stop))`: notes failed, engine unavailable or unreachable, cancel during notes | Failed (`RunFailure::status` is always Failed, 381) | 1517-1524, ~1727 |
| Running | `Err(Reason(Cancelled))` | Cancelled | 1517-1524 |
| Running | other `Err`, or a panic | Failed{engine} | 1509-1512, 1517-1524 |
| Pending or Running | launch after a quit or crash | Failed{Interrupted} | store.rs:6808-6873 |
| any | rewrite fails | Failed | session.rs:3605-3616 |
| any | rewrite receipt committed | Succeeded (same transaction) | store.rs:6229-6290 |
| any | rewrite receipt rejected (stale or conflict) | unchanged | session.rs:3618-3632 |

Swift mirrors:
- The status line (MeetingReviewView.swift:342-430) shows, in order: error, transcript deleted, remote cancellation pending, live update, audio deleted, then "Notes failed: …" with "Open Settings" and "Try again".
- The list row badge (MeetingsStore.swift:162-172) marks Failed and Cancelled as urgent.

#### M3. MeetingPhase, the notes slice
Owner: store.rs (`transition`, `allowed_actions`) and processing.rs (`finish_review` 1563-1645).

| from | event | guard | to | path |
|---|---|---|---|---|
| Stopping | stop | phase | Processing, then submit | session.rs:2318, 2405 |
| (new) | import | none | Processing, then submit | session.rs:2472 |
| RecoveryRequired | finalize partial | phase | Processing, then submit | session.rs:2900 |
| Processing | run ends | `allowed_from = [Processing]` only, no revision check | ReviewReady, or RecoveryRequired when origin is Recovery and the run did not complete | processing.rs:1563-1645; store.rs:3876-3918 |
| Processing | launch after a crash | phase | RecoveryRequired | store.rs:6808-6873 |
| Processing, ReviewReady, RecoveryRequired | delete or discard | phase | Deleting | store.rs:6340-6360 |
| Deleting | finish or restore | a restore bundle exists only from ReviewReady or RecoveryRequired | gone or restored | cloud_bundle.rs:280-290 |

Invariants:
- The "notes are ready" effects fire once, and only on Processing to ReviewReady: `record_meeting_finalized` (workflow_engine.rs:442), `after_notes_ready` (integrations/mod.rs:360), `present_wrap` (detection.rs:1613), `after_meeting_finalized` (automations.rs:171).
- The only other trigger is `after_notes_ready` when a meeting is filed into a folder (folders.rs:71).

Allowed actions (store.rs:8424-8456):
- Processing: Discard only
- ReviewReady: Edit, Regenerate, Export, Delete
- RecoveryRequired: FinalizePartial, Discard, Delete

Out-of-band events:
- An edit during Processing is allowed (store.rs:6979-6985).
- A delete during Processing makes `finish_review`'s transition fail with Conflict. The effects are skipped.
- A quit leaves the meeting in RecoveryRequired.

#### M4. Notes revisions
Owner: store.rs:5657-5715, 9029-9050; processing.rs:2526-2674.

States: Current with content, OutOfDate with content, Failed with no content. A Current row with no content is rejected as Invalid.

Combinations nobody named:
- Several Current rows at once. A newer Current never demotes an older one.
- A Current row whose content has no ledger: plain notes, a failed ledger, or rows from before ledgers existed.

Invariants:
- A key (evidence digest, input revision, template, prompt, model, intent, plus the operation id for a rewrite; 2526-2549) is written once, unless the earlier row Failed.

There are two "current" rules:
- "Newest current" is used by the Swift notes pane, core loops (store/loops.rs:683-702), sends and automations.
- "Newest current that has a ledger" is used by Swift `currentLedger` (MeetingTypes.swift:1184-1186) and the HTML export (session.rs:3748-3756).
- This is D3.

| from | event | to | path |
|---|---|---|---|
| (none) | the pipeline or a rewrite stores notes | Current; OutOfDate if the session revision moved since the input was read | store.rs:5657-5715 (5676-5682) |
| (none) | same key exists and is not Failed | returned unchanged (Cached) | processing.rs:2550-2562 |
| Failed | same key stored again | replaced | store.rs:5657-5715 |
| (none) | notes and plain notes both fail | Failed row | processing.rs ~2600-2625 |
| Current | any segment, note or speaker edit, call-name rename, cross-meeting correction, voice identity, relisten | OutOfDate (every row) | store.rs:9029-9050; callers store.rs:4853, 4967, 5011, 5044, 5079, 5131, 5152, 5361, 5630; call_speakers.rs:334; transcript_corrections.rs:989; voice_identity.rs:895, 1028; relisten.rs:195 |
| OutOfDate | (none) | never returns to Current | (none) |

After every stored revision, `derive_title_from_headline` runs (processing.rs:2670-2674). When the title is still "Local notes" (types.rs:60), it bumps the session revision (store.rs:4872-4930). `carry_loops_forward` follows (2675).

#### M5. Structured notes reading
Owner: `processing/complete_reading.rs`.

Each piece ends as one of: read, halved, left out, stopped, unreachable.

Rounds:
- Round 0 reads the transcript parts. Later rounds join what earlier rounds wrote.
- One section left means done. Any piece left out in a join round fails the whole reading (866).
- A join round that does not shrink is EvidencePack (890). There are at most 16 rounds.

Rules:
- Halving applies to an unstructured reply, a timeout or a rejection, at most twice (`MAX_SPLITS`, 125). A half that times out is not halved again (162-166). Stopped and Unreachable end the reading (213-218).
- A refusal (safety block or decline) is retried once under the short prompt (719-760).
- Local calls time out at 240 s (local_generator.rs:26). One piece can take up to about 720 s before it is left out.
- Budgets are read at 825-826 and 886-887.

#### M6. Plain notes fallback
Owner: processing.rs ~2600-2625; complete_reading.rs:955-1035.

- It is entered on any notes error except Stopped (returns the stop) or Unreachable (returns Unreachable).
- Round-0 failures are left out. Join-round failures keep the unjoined pack (1000-1006).
- If nothing at all was read, a Failed row is stored.
- The content is marked plain, and the ledger is skipped for plain content (~2627-2640).

#### M7. Ledger
Owner: complete_reading.rs:1203-1500; processing.rs:5769-5860; ledger.rs:335-395, 793-822.

- Each part goes through halving around `read_ledger_pack`. Receipts are retried once, then unverified rows are degraded.
- Section count:
  - 0 is an error
  - 1 is used as is
  - 2 or more go to one merge call (1397-1440): three shrinking inputs, else EvidencePack (1203-1265)
- The merge reply must parse and carry a headline. `apply_merge` follows (1267-1395).
- Any merge failure fails the whole ledger and sets the failure cause.
- Cause mapping (processing.rs ~5838-5845): Stopped becomes EvidencePack, Rejected becomes ReplyRejected, and an engine error keeps its own cause. An unreachable engine reads as a refusal, deliberately, per the comment there.
- Failed checks become caveats and never reject the ledger.

#### M8. Engine choice
Owner: processing.rs:758-768, 1051-1162; local_generator.rs; relay_generator.rs.

- The engine is chosen once per run (1689) or per rewrite (1334-1335).
- The relay is chosen only when remote is on, the relay is reachable and the series is not opted out. A read error on the opt-out counts as opted out. Otherwise Local if available, else None.
- Local availability is cached for 2 s (local_generator.rs:25). The generator is cached per settings value, and that cache includes errors (1088-1103).
- Without a configured context window, the context comes from the model catalog (218-227). The model info re-probes after the 2 s cache (181-211) with a 2 s probe timeout (llm_client.rs:466, 677-701). The result is `Some(…unwrap_or(0))` (295-298).
- Relay turns have a 660 s deadline. A join timeout of 695 s becomes Unreachable. Nothing rechecks consent (relay_generator.rs:236-265).
- A settings change has no hook into running jobs (settings.rs:3118-3126).

#### M9. Swift mirrors
Owner: MeetingsStore.swift, MeetingTypes.swift, MeetingReviewView.swift, MeetingNotesPane.swift, MeetingLiveStore.swift.

- `pending` is one slot shared by every action, and `busy = pending != nil` (1393). Every `act` sets it, clears `error`, and clears `pending` when it finishes (2068-2090). Whichever action finishes first clears it for all of them (D2).
- `enhancing` is set during a rewrite (1943-1955).
- `error` is a single slot. A refresh happens only on a stale revision.
- Loops reload when `revision:currentArtifact.generationKey` changes (1608-1610, 1656-1668).
- Notes selectors: `currentArtifact` (MeetingTypes.swift:1189-1191). `readableArtifact` (1197-1199) falls back to out-of-date notes.
- Ledger selectors: `currentLedger` (1184-1186), `readableLedger` (1202-1208), `ledgerFailure` from the newest readable revision (1212-1214).
- The notes pane has no revision picker (MeetingNotesPane.swift:146-160).
- The record-page engine warning (MeetingLiveStore.swift:244) reloads only on a settings change, on app activation, and at start (405-421, 496-502).

#### M10. Run progress
Owner: processing/progress.rs.

- Stages: Transcribe, Diarize, Notes, People. `finish_progress` always publishes `progress: None` (3311).
- A rewrite publishes no progress.
- Swift shows progress only while the phase is Processing or the stage is People (MeetingsStore.swift:463-466).

## Settings and migrations

### After the audit fixes

Format as in MapSettings.md. Paths are under `src-tauri/src` unless named.

#### M1. The settings file on disk (replaces the map's M1)

**Owner:** settings.rs `SettingsFile` (2338-2386). The plugin only loads the file.

**States:**
- S0 absent: a fresh install, or after an identity rollback parked the file.
- S1 current. S2 older schema. S3 newer schema. S4 valid JSON, one field rejected.
- S5 torn, empty, or not a JSON object.
- S6 a JSON object without the `"settings"` key.
- S7 set aside: kept as `settings_store.json.unreadable-<ms>`, never read again by Sona.
- S8 retired: the rollback moved it out; nothing in that run writes it again.

**Invariants:**
- I1: a write is a new file, synced, renamed over the old one (fs_util.rs:102-127, called from settings.rs:2373-2385). A reader sees the old or the new file, never part of one. The temp name carries the pid because the app and a CLI run write the same file (fs_util.rs:107-110).
- I2: a file that is not a JSON object is never overwritten (2357-2360, 2388-2416), and that run knows it (`UnreadableSettings`, 2328-2332).
- I3: the plugin never writes this file. Auto-save is off and its serializer refuses (2361-2365); the plugin serializes before it opens the file (plugin store.rs:292-299), so its exit-save (plugin lib.rs:448-457) cannot truncate it.
- I4: the version number is only ever raised (unchanged).

**Transitions:**

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| S0, S6 | first settings read in any process | no `"settings"` key | S1 fresh defaults, saved | settings.rs:2501, 2539-2549 |
| S5 | first open of the store in a process | not a JSON object | S7 (renamed, error logged), `UnreadableSettings` for this run, then as S0 | 2357-2360, 2388-2416 |
| S2 | first read | a migration or heal step changed something | S1, saved | 2511-2536 |
| S4 | first read | typed parse fails | salvaged document, saved, `warn!` | 2503-2508, 2604- |
| any | typed write | not retired | whole document to `<path>.<pid>.tmp`, fsync, rename | 3496-3508 → 2373-2385 → fs_util.rs:102-127 |
| any | typed write | retired (S8) | refused; memory keeps the change; caller hears `Save` error | 2374-2378 |
| any | clean exit | — | unchanged; the plugin's exit-save is refused before it opens the file | 2364 |
| any | `process::exit`, SIGTERM, SIGKILL, crash, power loss | — | the last complete file; at most a stray `<path>.<pid>.tmp` | fs_util.rs:118-126 |
| S1 | "Put the adopted data back" | Completed receipt, legacy app closed | S0 on disk, S8 for the rest of the run | identity_adoption.rs:979 → settings.rs:2418-2429 |

#### M2. Each process's in-memory copy of the store (changed rows only)

- Unloaded → loaded at the first `SettingsFile::open` (settings.rs:2349-2371). A process that loads it sets an unreadable file aside first (2358).
- Loaded → saved only by Sona's own write (2373-2385). There is no debounced save (`disable_auto_save`, 2363).
- Exit writes this file only to land a save that failed and has not landed since (`save_owed_settings`); otherwise Exit and Drop never write it, and a file moved out of Sona's folder is never written again.

#### M5. Settings generation (replaces the transitions of the map's M5)

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| R | `update_settings` / `try_update_settings*` | closure returns Ok | R+1, also when the disk refuses (memory took it) | settings.rs:3536-3581 (bump 3544) |
| R | bookkeeping stamp: agent connection time, verified provider key | — | R unchanged; saved; "settings-changed" emitted | 3627-3675 |
| proposal Pending | Apply | revision matches | Applied with receipt (revision, undo); runtime effects run; ActionFailed returned when the save failed | agent_panel/config.rs:445-478; agent_panel/mod.rs:1589-1599, 1640-1673 |
| proposal Pending | Apply | revision stale | Rejected, receipt dropped | config.rs:603; mod.rs:1593-1596, 1736- |
| proposal Applied | Undo | receipt revision matches | Undone; runtime effects run; event emitted; ActionFailed returned when the save failed | config.rs:490-503; mod.rs:1700-1733 |
| proposal Applied | Undo | revision stale | Rejected, receipt dropped (unchanged) | config.rs:511; mod.rs:1704-1707 |
| R | revert of the legacy settings import | — | backup's R + 1 (unchanged; see E4) | upstream_import.rs:326; settings.rs:3544 |

#### M7. Legacy settings and history import (replaces the map's M7)

**How imported history is identified:**
- The identity hashes the label, the canonical root and the creation time of the legacy `history.db`; not its length (upstream_import.rs:733-752).
- A row counts as imported when (source id, identity) or (source id, row hash) matches (managers/history.rs:1290-1311), backed by the index `upstream_import_history_source_row` (managers/history.rs:269-274). The lookup runs before any recording is copied (upstream_import.rs:640-675).

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| any | import with Settings ticked | `settings_imported` is false | backup written; merge; receipt says imported inside the settings write, before the save (kept on purpose: a crash leaves Undo restoring the same settings) | upstream_import.rs:212-273 (backup 257, receipt 263-266) |
| any | import with History ticked | legacy app closed; held unsaved rows + new unsaved rows − limit in force ≤ removals the person agreed to (0 unless they chose "Import anyway") | history imported; matches by identity or row hash count as existing | 340-419, 478-, 640-675 |
| any | import with History ticked | held + new unsaved rows − limit > agreed removals | asks with `history_would_remove_older { removals }`; nothing inserted (settings may already be imported); "Import anyway" runs the import again agreeing to that count, and a count that grew since asks again | 664-673 |
| any imported | import after the legacy app was used | same `history.db` | same identity; only new rows | 733-752 |
| any imported | import after `history.db` was replaced | identity differs | history counts reset; settings part, backup and Undo kept | 134-144, 355-358 |
| settings imported | "Undo the settings import" | backup readable | memory = backup; shortcuts and modes follow; save ok → receipt "not imported", then backup removed; save failed → receipt and backup unchanged, error returned | 284-297, 307-336 |

#### M8. Identity adoption (replaces the map's M8)

**States:** no receipt; waiting on the person (alert shown, `<socket>.waiting` present); in progress (journal entries planned, some done); final receipts Portable, NothingToAdopt, SkippedNonvirgin, FreshStart, Completed; rolled back.

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| none | start of a process that reaches setup | receipt exists | no-op | identity_adoption.rs:180-182, 318-320 |
| none | start | portable | Portable | 183-188, 321-325 |
| none | start | no legacy folder | NothingToAdopt | 189-199 |
| none | start | no journal entries; nothing to move | NothingToAdopt | 326-337 |
| none | start | no journal entries; Sona's `settings_store.json` exists | SkippedNonvirgin (final) | 338-347 |
| none, in progress | start without the shell (CLI, agent tool) | — | unchanged; setup fails with `choice_pending`; nothing written | lib.rs:2200-2205; identity_adoption.rs:207, 350 |
| none, in progress | start under the shell | — | waiting: alert on the setup thread; marker file; the shell's launch window restarts while the marker exists | 206-207, 225-246, 251-300; macos/Sona/Core.swift:331-333 |
| waiting | "Start fresh", or the alert cannot be shown | — | FreshStart (final) | 294-298, 352-361 |
| waiting | "Retry" | legacy app running or unverifiable | waiting again | 297, 363 |
| waiting | "Move data" | legacy app closed when the alert opened | in progress; plan written once, settings first | 362, 368-374 |
| in progress | an entry fails | — | the entry keeps its plan (Renamed or Copied; no failed state exists); error returned; the core exits | 118-132, 375-391 |
| in progress | next launch | journal has entries | resumed: the two exits above are skipped, the alert asks again, done entries are skipped, the failed entry's plan runs again | 326-327, 338, 375-378, 481-528 |
| in progress | copying an entry | a temp left by an earlier attempt | the temp is replaced | fs_util.rs:40-51 |
| in progress | all entries done | — | vocabulary, credentials, autostart, then Completed and the tombstone | 393-420 |
| Completed | "Put the adopted data back" | mode Completed, legacy app closed | rolled back; settings file retired for the rest of the run; the caller ends the process | 959-1025 (967-972, 979); settings.rs:2418-2429 |

#### M10. History retention (one new row)

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| any | sweep after a saved entry or at launch | this run found the settings file unreadable | no deletion; `warn!` logged | managers/history.rs:1941-1951 |

### As mapped

#### M1. The settings file on disk (`settings_store.json`, key `"settings"`)

**Owner:**
- tauri-plugin-store: load at plugin store.rs:222, save at plugin store.rs:292-299.
- settings.rs drives it: read at 2393-2467, write at 3396-3426 and 3436-3480.

**States:**
- S0 absent: a fresh install, or after an identity rollback parked the file.
- S1 current: schema 20 and it parses.
- S2 older: schema below 20. A missing version reads as 0 (settings.rs:2622-2625).
- S3 newer: schema above 20, written by a newer build.
- S4 field-invalid: valid JSON, but `AppSettings` rejects one field.
- S5 torn, empty or not JSON.
- S6 JSON without the `"settings"` key.

Combinations nobody named:
- S3+S4 is an older build reading a newer file. It is salvaged and keeps S3's version number.
- S5 and S6 look exactly like S0 to Sona, because the load error is swallowed (plugin store.rs:222).

**Invariants:**
- I1: a write replaces the whole file in place, truncate then write (plugin store.rs:296). There is no temp file and no rename.
- I2: a load never surfaces an error (plugin store.rs:222).
- I3: every process that loaded the store writes its own copy back at a clean exit (plugin lib.rs:448-457).
- I4: the version number is only ever raised (settings.rs:2843-2846).

**Transitions:**

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| S0, S5, S6 | first settings read in any process | `store.get("settings")` is None | S1 fresh defaults, saved; nothing is logged | settings.rs:2399, 2437-2447, 2460-2464 |
| S2 | first read | a migration or heal step changed something | S1, saved | 2409-2435, 2449-2458, 2460-2464 |
| S4 | first read | typed parse fails | salvaged document, saved, only a `warn!` | 2400-2407, 2504-2536 |
| S3 | read by an older build | parse ok | stays S3; unknown keys dropped at the next write | 2843-2846, 2347-2364 |
| S3+S4 | read by an older build | parse fails | salvaged, newer version kept, saved | 2404-2405, 2504-2536, 2843-2846 |
| any | typed write | — | rewritten in place | 3436-3480 → 3396-3426 → plugin store.rs:296 |
| any | clean exit of a process that loaded the store | — | rewritten with that process's memory | plugin lib.rs:448-457; tauri app.rs:1430-1433 runs plugin handlers first |
| any | `process::exit` (CLI), SIGTERM, SIGKILL, crash | — | unchanged since that process's last save | lib.rs:2237, 2278; Core.swift:245; cli.ts:109 |
| S1 | identity rollback | Completed receipt | S0 (file moved to a backup folder) | identity_adoption.rs:893-958, 832-839, 870 |

**Out-of-band events:**
- A power loss or kill during `fs::write` leaves S5.
- A full disk after the truncate leaves S5: the write returns an error and the file stays empty or partial.
- A hand edit while Sona runs is overwritten by the next save or the exit-save. Nothing in src-tauri/src calls `.reload(`.
- Two processes can write at once. The only lock is in-process (settings.rs:29-35).

#### M2. Each process's in-memory copy of the store (plugin cache)

**Owner:** the plugin's `Store`, one per process per path.

**States:** unloaded → loaded and clean → loaded with a pending debounced save → saved.

**Invariants:**
- The file is loaded once per process and never re-read.
- Every Sona `set` is followed by an explicit `save`, which cancels the 100 ms debounce (plugin store.rs:68, 545-548).
- Drop applies any pending save (610-613).
- The exit-save runs even when nothing changed (plugin lib.rs:448-457).

**Writers, each with its own copy and no lock between processes:**
- The GUI core `sona-core`, started by Swift (`Core.start`; `Self.end(previous)` at Core.swift:209).
- Every CLI run (lib.rs:1022-1028):
  - `--query` runs once per agent tool call, because the MCP server spawns a new process each time (cli.ts:3-5, 104-106). Its default binary is the installed app's `sona-core` (cli.ts:11-12).
  - transcribe, list-models, list-devices.
  - Running these alongside the app is deliberate: "Headless CLI/MCP commands retain their existing concurrent path" (lib.rs:1463-1467). The single-instance plugin is also skipped for these runs (lib.rs:2103-2104).
- Any branch or dev build. They share the same data folder:
  - there is one identifier (tauri.conf.json:4)
  - portable mode needs a marker file, and debug builds also need `SONA_ALLOW_PORTABLE_DEV=1` (portable.rs:89-92, 215-216)

**Transitions:**
- Unloaded → loaded at the first `app.store()`: lib.rs:2204-2206 → settings.rs:2331-2345.
- Loaded → saved on each typed write.
- Always saved at Exit.
- CLI processes leave through `process::exit` (lib.rs:2237, 2278). They save only through writes made while reading (settings.rs:2460-2464), or through `ModelManager::new` (lib.rs:2251 → model.rs:1970-2040, called at 1553).

**Out-of-band events:**
- The core's own quit runs the exit-save: native "shutdown" calls `app.exit(0)` (native_bridge/mod.rs:343-345), and so does the parent-gone path (226-228).
- Swift's `end()` sends SIGTERM after 3 s (Core.swift:158, 239-247). That skips the exit-save.
- Only SIGUSR1 and SIGUSR2 are handled (signal_handle.rs:43, 47).

#### M3. Typed settings cache

**Owner:** settings.rs: `SettingsCache`, `forget_cached_settings` at 59-62, `get_settings_locked` at 2371-2391.

**States:** None, Some(settings).

**Invariant:** Some always equals `read_settings_from_store` of the current in-process store. Every writer clears it under the lock (3410-3426; raw mutators at 2483-2497).

**Transitions:**
- None → Some on read. Each refill re-runs the migrations, salvage and heal steps on the in-memory document.
- Some → None on every write.

**Out-of-band:** a failed disk save leaves memory ahead of disk. This is pinned as intended by the test at settings.rs:4477-4495.

**Verdict:** consistent inside one process. No candidate.

#### M4. Schema ladder

**Owner:** `apply_settings_migrations`, settings.rs:2598-2870.

**States:** versions 0 through 20, plus anything above 20 (unknown).

**Transitions:**
- Each `< N` step runs once.
- There are no steps for 6 or 16. That is harmless with `<` comparisons.
- The `<13` step resets the revision to 1, once (about line 2726).
- The `<20` step changes `history_limit` 5 → 100 and marks the document updated unconditionally (2834-2842).
- The version is stamped only if it is lower (2843-2846).
- The overlay step comes after the stamp (about 2858-2870).
- The Handy decoder and the backup decoder run the whole ladder, then stamp the current version regardless (2919, 2935).
- The import merge stamps the current version (2968).

**Invariant:** a document is migrated once. Nothing repairs a document whose version is already current or newer.

**Out-of-band:**
- An older build writes a newer file (E10).
- A Handy document is run through the whole ladder (2918), including the schema-20 step (E5).

#### M5. Settings generation (`settings_revision`)

**Owner:** settings.rs:3436-3480. The bump at 3444 applies to the value after the update closure has run.

**Consumers:**
- config.rs: `apply_changes` at 448-460 checks at 579; `undo_changes` at 472-484 checks at 487.
- mod.rs: `apply_proposal` at 1551-1600 with a pre-check at 1569-1573; `undo_change` at 1673-1730; `record_config_error` at 1732-1755.

**States:** revision R. Proposals go Pending → Applied → Undone, or end up Rejected.

**Intended invariant:** R moves only when a user-visible setting changes. The connection stamp is exempt on purpose (settings.rs:3532-3550): "Persist a connection receipt without changing the generation settings proposals compare against."

**Transitions:**
- Every `update_settings` and `try_update_settings` adds 1 (3444). That includes bookkeeping such as `mark_post_process_secret_verified` (3551-3561), called from `credential_proved` (actions.rs:872-874).
- Reverting the Handy import sets R to the backup's R plus 1 (upstream_import.rs:285, then 3444). R can go backwards.
- The import merge keeps the target's R (2967), then adds 1.
- An apply with a stale R fails as StaleRevision, and the proposal is marked Rejected with its receipt dropped (mod.rs:1740-1741).
- An undo with a stale R does the same (mod.rs:1699-1703, then 1740-1741).

**Out-of-band:** any cloud-cleaned dictation between a proposal and its apply, or between an apply and its undo (E12).

#### M6. Legacy provider secret journal

**Owner:** settings.rs:2331-2364, plus secrets.

**States:** not pending, migration pending, cutover pending.

Re-serializing keeps the legacy secret keys (2347-2364). Checked and benign.

#### M7. Handy import (receipt plus settings backup)

**Owner:** upstream_import.rs.
- The files are the receipt and the pre-import settings backup. Their names are at identity_adoption.rs:22-23.
- The source is `com.pais.handy` (upstream_import.rs:24), root at 746-750.

**How imported history is identified:**
- The identity is a hash of the label, the canonical root and the byte length of Handy's `history.db` (632-645; the length is at 641-643).
- History is deduplicated on (identity, source id) (history.rs:169-176, 1219-1240).
- Imported recording names embed a 12-character prefix of the identity (upstream_import.rs:513-514, 665).

**States:**
- not imported
- settings imported, with the backup present
- history imported under identity X
- both imported
- settings reverted, with the backup removed
- identity changed, so the next import resets the receipt

**Transitions:**

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| any | import with Settings ticked | `settings_imported` is false | settings imported; backup written; receipt written before the settings save | 197-258 (guard 203-205, backup 242, merge 243, assign 247, receipt 248-251) |
| any | import with History ticked | Handy is closed | history imported under the current identity | 333, 334-359, 372 |
| any imported | import after Handy's `history.db` changed size | identity differs from the receipt | receipt reset to default, then the import runs as if it were the first | 317-322 |
| settings imported | "Undo the settings import" | backup readable | whole document = backup; receipt written; backup removed after the save | 271-299 (278, 285, 289, 295-297) |

**Intended invariants:**
- The receipt matches disk.
- The backup exists exactly when settings are imported and not reverted.
- Re-importing history adds only new entries.

**Out-of-band:**
- A crash between the receipt write and the settings save (E13).
- Handy used between imports (E2, E3).
- The Privacy page selection is seeded once per session (PrivacyStore.swift:489-505).

#### M8. Identity adoption (old app identity → Sona)

**Owner:** identity_adoption.rs. The receipt and the journal live in Sona's folder. The tombstone `moved-to-sona.json` (line 19) goes in the legacy folder.

**States:**
- no receipt yet
- in progress: journal entries planned, some done
- final receipts: Portable, NothingToAdopt, SkippedNonvirgin, FreshStart, Completed
- rolled back: receipt, journal and tombstone deleted; settings and history parked in a backup folder

**Transitions:**

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| none | start of any process that reaches setup, GUI or CLI | receipt exists | no-op | lib.rs:2198-2201; identity_adoption.rs:154-156 |
| none | start | portable | Portable | 157-162, 247-251 |
| none | start | no legacy folder, or nothing to move | NothingToAdopt | 163-173, 252-261, 355-360 |
| none | start | Sona's `settings_store.json` exists | SkippedNonvirgin (final) | 262-271 |
| none | dialog "Start fresh", or the dialog thread panics | — | FreshStart (final) | 198-231 (`unwrap_or(false)` at 223-224), 275-284 |
| none | "Move data" with the legacy app closed | — | in progress; settings is first in the plan | 285, 292-297, 369-376 |
| in progress | an entry fails | — | entry marked Failed; error returned; the core exits | 316-319 |
| in progress | all entries done | — | vocabulary, credentials, autostart, then Completed and the tombstone | 324-351 |
| Completed | Undo on the Privacy page | mode is Completed | rolled back; "the caller ends the process" | 893-958 (901-903, 948-955) |

**Intended invariants:**
- An interrupted move resumes from the journal.
- From the user's view, the move happens fully or not at all.
- After a rollback, Sona's folder is fresh.

**Out-of-band:**
- A kill during the move: Swift's "Try again" sends SIGTERM after 3 s; the MCP server sends SIGKILL at 30 s; or a crash.
- A CLI process doing the move. There is no guard for CLI runs in 151-196.
- The core's exit-save after a rollback.
- Two processes moving at once.

#### M9. Swift mirrors

**Owner:** the stores in macos/Sona. Each one observes "settings-changed":
- AboutTypes:11, AgentBridgeTypes:13, ChatTypes:107, LibraryStore:9
- MeetingLiveTypes:39, MeetingSettingsTypes:16, ModesStore:9, PrivacyTypes:10
- PromptsTypes:11, ProvidersStore:6, SettingsTypes:9, VocabularyTypes:13

**States:** not loaded, loaded snapshot, stale until the next event.

**Invariants:**
- Swift writes settings only through core commands.
- Every core write emits "settings-changed" (settings.rs:3474), even when the disk save failed. The connection stamp emits too (3542-3545).
- Every store re-reads on that event.

**Out-of-band:** a core restart. The new core reads disk, and the stores reload on reconnect.

**Verdict:** no candidate. One cost to note: every cloud-cleaned dictation makes all twelve stores re-read settings (E12).

#### M10. History retention

**Owner:** history.rs:
- `cleanup_old_entries` at 1917-1958
- `sweep_by_count_with_connection` at 2041-2066
- `expire_recordings_with_connection` from 2074

It runs after every saved entry (1622) and once per launch (lib.rs:2395-2397).

**States:**
- at or under the limit: stable
- over the limit: the next trigger deletes
- recordings past the retention cutoff

**Invariants:**
- Keep the newest N unsaved rows by timestamp (2051-2052).
- Also keep saved rows, and empty cancelled or interrupted dictations for 14 days (399, 404-407).
- Deletion removes rows and audio. It cannot be undone.

**Inputs that change N or the cutoff:**
- the Library control (LibraryStore.swift:645-662, no confirmation; it shows `limit ?? 100` at 631-634)
- the schema-20 step
- fresh defaults or salvage (E1, E15)
- the Handy settings import (settings.rs:2992-2993)
- reverting that import (upstream_import.rs:285)

**Out-of-band:**
- Every path except the Library control can push the machine over the limit without asking.
- Imported rows keep their old timestamps.

## Agent panel

Chat review cards, after the audit fixes (`agent_panel/mod.rs`).

| From | Event | Guard | To | Code |
|---|---|---|---|---|
| no turn, or a terminal turn with no action in progress | send_turn | `!turn_holds_sheet` | new turn Submitting, cards empty | mod.rs:817, 875 |
| non-terminal turn, or any turn while apply/undo/edit holds `action_operation` | send_turn / open_conversation / new_conversation | `turn_holds_sheet` | unchanged; TurnActive | 486-492, 817-819, 585-587, 606-608 |
| same | record_exchange (voice command answer) | `turn_holds_sheet` | unchanged; exchange written to history only | 632-642 |
| terminal turn, no action in progress | open_conversation / new_conversation / record_exchange | `!turn_holds_sheet` | no turn; conversation replaced | 591, 612, 646 |
| turn Succeeded with an answer | relay job accepted | response has actions | each card Pending | 2199 |
| card Pending | apply_action | holds `action_operation` for the whole call; `to_run` is Some | mutation runs with panel lock released; turn cannot be replaced meanwhile | 1368, 147-154, 1382 |
| card Pending | mutation returns Ok / Err | turn still present (guaranteed by the row above) | Applied(receipt, undo) / Failed(detail) | 1383-1384, 1447-1475 |
| card Applied / Dismissed / Failed | apply_action | `to_run` is None | unchanged; current status | 1373-1375 |
| card Applied | dismiss_action | holds `action_operation`; `reversal` is Undo | inverse runs, then Dismissed | 1404-1422 |
| card Pending / Failed | dismiss_action | `reversal` is Unapplied | Dismissed | 1404-1422 |
| card Pending | edit_action (Review sheet save) | holds `action_operation`; same action kind; validates against the turn's context pack | Pending with the edited action | 1477-1505 |
| any | edit/apply/dismiss on an unknown turn | turn id does not match | unchanged; UnknownTurn | 1436, 1460, 1489 |

Bridge row (integrations/mod.rs:654): `connections_edit_chat_action` reads the managed `AgentPanelManager`; every request gets a reply.
