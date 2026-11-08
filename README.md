# Sona

Sona is a local-first Mac and iPhone app for spoken words: dictation delivered into other apps, and meeting notes recorded, transcribed, and remembered. The desktop app is native SwiftUI on macOS 15 or later. The native iPhone app includes a Watch companion and a dictation keyboard; there is no separate web desktop app or Windows/Linux release.

Sona is built on [Handy](https://github.com/cjpais/Handy) by CJ Pais, an open-source speech-to-text app, and keeps its MIT license. See `NOTICE` for the other projects it draws on.

## What Sona does

### Dictation

- Local Whisper-family GGUF and ONNX transcription models.
- Per-mode recognition, context, prompt, delivery, and shortcut settings.
- Spoken edits ("scratch that", "make that a list", "quote that") and a "Sona," cue that turns the end of a dictation into an instruction for the mode's AI cleanup.
- Optional cloud transcription with a user-provided provider key and explicit consent.
- AI cleanup on Apple Intelligence or a provider you configure: OpenAI, OpenRouter, Anthropic, Groq, Cerebras, or any OpenAI-compatible server. English dictation also offers a downloadable offline model on this Mac, with download progress, cancellation, retry, and removal in Settings > Providers. It supports light punctuation and filler removal, not custom tones or spoken instructions; an incomplete rewrite or a change to words or numbers keeps the original text.
- Searchable local history with delivery receipts and retained recordings.
- Audio-file transcription, vocabulary corrections, CSV vocabulary tools, and optional emoji replacements.
- Streaming dictation retains queued opening audio when recognition falls behind. Microphone readiness follows the first audio sample, including a Studio Display microphone, rather than a full scan of its supported formats.
- Context capture from the focused field on your Mac, under a global ceiling. At Off, dictation reads nothing from other apps except the text you select for a command.

- A compact recording pill stays the same size while listening, processing, and reporting delivery. A checkmark means macOS confirmed the paste; an arrow means the input was sent but could not be confirmed.
- In Dictation settings, turn off the idle pill separately from recording controls. Always-on capture and the thirty-second microphone hold are separate switches. With the idle pill off, delivery feedback fades out without switching back to a microphone.

### Meetings

- Records the microphone and system audio of a meeting after a consent step, with no bot joining the call. Detects meetings from the calendar, from the meeting app you are in, from a call tab in Safari, Chrome, Edge, Firefox, or Arc, and from FaceTime and Phone calls. Presence is not participation: an open but untouched meeting app never prompts, and the status line says why. Calls in apps you grant can record automatically.
- Transcribes after the meeting, with diarization, notes, a ledger of every thread with its verbatim receipt, people, and series memory; catch-up and questions also work while the meeting is still running. Long transcripts are read in bounded parts and reconciled without cutting off their ending. Notes become ready before optional people and relationship summaries finish. A finished meeting opens on its ledger.
- Speaker labels: label an unresolved voice as an existing or new person, or correct a wrong one. Renaming a speaker in the transcript names its voice the same way; renaming it back to its Speaker label takes the name off the voice. In a call where Sona records the other side's audio, your microphone lines carry your name from About me. Sona keeps a voice fingerprint of every speaker it separates in a meeting, on this Mac, and uses them to suggest who an unlabeled speaker is from the people you named in other meetings. A suggestion names no one until you confirm it. When other meetings heard the same unlabeled voice and nobody named it there either, the label sheet lists them with a clip of each, and one name labels the voice in every meeting you leave checked. A fingerprint nobody names is deleted 90 days after the meeting is processed, unless it sounds like you or someone you named. Marking a speaker unknown, correcting it, forgetting a person, or deleting or expiring the transcript removes those fingerprints. Naming a voice automatically in later meetings is a separate opt-in that stays on this Mac. It uses at least four seconds of retained, contiguous local microphone or call audio; imported recordings cannot enroll voices. A person's page can forget the profile. Saved voices are used only with the same model and audio preparation that created them. Older profiles remain stored but do not name new speech automatically.
- Live transcript voices are provisional acoustic labels, scoped separately to the microphone and call audio. They are not guessed identities or a promise of final diarization. Short or unclear evidence stays unresolved, and an unavailable voice model is shown explicitly.
- Call participant names: while recording call audio, open **Names from this call** and choose a Zoom, Teams or Google Meet call. Reading and automatic naming both start off. Readable participant names become review choices; a clear speaking-time match can suggest a name without replacing one you typed or a remembered voice. This needs macOS Accessibility access and a supported English people panel. Missing or ambiguous speech indicators never become a guessed name. Call-name evidence stays on this Mac and is removed when its transcript is deleted.
- What was open during a call: while a meeting records, Sona notes the windows in front, with the app, the window title, the file behind it, and the website. Full page addresses are kept only when Include browser URLs is on. Windows with a password field in focus, private browser windows, and Sona's own windows are skipped. Review offers the list under **Open during this call**; attached items become the meeting's documents or links. Dismiss deletes the rest, and anything not attached is deleted after seven days. Control-Option-W attaches the window in front right away. The list stays on this Mac, and turning it off in Meeting settings deletes everything not attached.
- Imports a recording or a Granola, Otter, or Circleback transcript export as a meeting.
- Saved prompts with optional JSON-schema output, run by hand or after every meeting in a series or folder. Folder defaults allow one notes template and up to five meeting prompts.
- Folders group meetings without moving or copying them. Add a meeting to several folders, filter the library by folder, or start a fresh chat limited to that folder's meetings. Deleting a folder leaves its meetings intact.
- Export the whole meeting library as CSV from the Meetings page menu: title, UTC date, recorded duration, participant labels, summary, action items, and transcript. The export ignores list filters and excludes trash.
- Follow-up drafts that open in Mail, Reminders with due dates, and a thirty-day undo bin for deleted meetings. Delete forever removes a trashed meeting immediately after confirmation.
- Recording notices are off by default. In Meeting settings, edit the notice and enable posting for new recordings. The detected-meeting prompt can opt out for one meeting. Sona attempts a send once only after identifying a joined English Zoom meeting, its own empty chat composer, Everyone as the recipient, and the Send action. Unclear chats and other apps show “Not posted” with the exact notice to copy. Sona never uses a generic focused field, overwrites an existing draft, or retries an uncertain send. This notice does not replace participants' consent.
- The optional Sona Camera extension adds “Recording with Sona” to outgoing video only while Sona is recording. Install it from Meeting settings, approve Camera Extensions in macOS, allow camera access, choose a physical camera, then select Sona Camera in the call app. Installing it does not start video. Stopping or pausing the recording removes the label while normal video continues. The extension stops using the physical camera when its last video consumer closes, and never stores video. The signed native app must be in Applications; both the app and extension require the same Apple team and shared app group.
- Meeting intelligence runs on Apple Intelligence by default. Settings > Advanced also offers a local OpenAI-compatible endpoint, such as Ollama, for summaries, ledgers, recaps, and answers. Supply its loopback URL (including /v1), model ID, and context window if its model catalog does not report one. “Use cloud AI for meeting notes” is off by default. Turning it on sends meeting text through your paired server to its AI provider; audio stays on this Mac. Selected recurring series can stay local. Turning it off keeps new summaries, ledgers, recaps, and answers local.

### people, conversations, and notes

- home connects people, conversations, and notes. a person’s page shows their contributions and links back to each conversation.
- a portrait comes from a file you choose, a contact you select, or a public image address you approve and credit. the original image and its source stay with that person. sona never guesses whose face it is.
- conversation context distinguishes huddles, calls, in-person conversations, and note sessions. sources stay visible. a chosen location can show a map.
- learned phrases apply only to the person you identify. you can review, edit, or remove them. the original transcript stays available.
- you can teach sona your voice from your recent dictations under settings > meetings > about me. it keeps one voiceprint on this mac and only suggests which meeting speaker is you; you confirm each suggestion. forget my voice deletes it.
- notes link to people, conversations, and other notes. local synthesis combines selected records into a new note with source links and saved evidence. originals stay unchanged.
- agents can search notes, read their history, compare versions, and get corpus statistics through the consent-gated cli and memory tools.

mac notes and manual review of two to twelve notes work without microphone access or a conversation store. a review that includes conversations needs the normal credential store. local ai synthesis also needs a ready model and does not send sources to cloud ai.

voiceprints and correction rules stay on the mac. a paired iphone can read an encrypted snapshot of saved people, current notes, confirmed speaker assignments and bookmarks. it receives corrected wording for synced conversations without replacing the original words. the snapshot is read-only and contains no audio. a snapshot over 8 mib is refused rather than silently dropping records.

when the original conversation is deleted or its transcript expires, its saved evidence is removed too. the note stays.

### Projects and calendar

- Projects are private plans on this Mac. Milestones and tasks can have dates. Selecting a person for a task records your choice; it does not invite or notify them.
- Link a saved meeting, note or commitment to a project, milestone or task without copying the original. A person's Work view separates tasks you selected from confirmed conversation context.
- The calendar shows dated work alongside available meetings. When conversations cannot be read, it still shows dated work and says what is missing.
- In Chat > Plan, type or dictate a goal and optionally select saved sources. Prepare a local draft, review its sources, edit the plan and press Create project to save it. Existing meeting commitments stay linked work, not duplicate tasks.
- review links checks only the sources you select. its suggestions remain possible connections until you link or dismiss them; linking adds evidence, not a task assignment or status change. nighttime review stays off until you select sources for a project and turn it on. if sona stays open and the mac is awake and idle for 10 minutes, macos can schedule a review between 22:00 and 06:00 local time, though it may delay or skip it. it uses apple intelligence on this mac and saves only possible links for your review.
- The CLI and MCP tools can read saved projects after you turn on Settings > Agents > External access. They cannot edit projects.

### Screen recording

- Records a window or screen you pick in the macOS picker, with an optional camera picture-in-picture and microphone, to an H.264/AAC MP4 in `recordings/`. Pause, resume, stop and save, or cancel. Requires macOS 15 or later. The pick itself grants that recording, so it needs no Screen Recording access; it asks for Camera and Microphone access only when you turn those on. It does not capture system audio, and it refuses to start while a dictation or meeting holds the microphone.

### Agents

- A chat that answers from your corpus. Once you allow it to send matching quotes to your server, it can search recordings, read a meeting or transcript, look up a person, check open loops and the calendar, and count words and activity, at most three lookups per question, each shown as a step. It can also offer changes (close a commitment, assign an owner, rename a speaker, add a vocabulary term) that apply only when you press Apply, each with a receipt and an undo.
- In native macOS chat, choose ChatGPT or Claude, then a model and one of its supported thinking levels. The paired server supplies the model list. `Use server default` clears the saved choice.
- A `sona` CLI and MCP server for reading notes, note versions and diffs, statistics, meetings, people, loops, and the upcoming calendar, plus a consent-gated write to close a loop. `skills/sona/SKILL.md` documents the surface and JSON schema version 4.
- Local agent hooks for Claude Code, Codex, Grok, and OMP, with permission requests answerable from Sona where the tool has a reply channel.

### Phone and watch

- `mobile/` holds one iPhone app with an embedded Apple Watch companion and keyboard extension. They pair with your Mac through the same end-to-end encrypted vault the desktop syncs through, record in-person conversations, and hand them to the Mac as meetings. The iPhone app requires iOS 26; its Watch companion targets watchOS 10 or later. On iPhone, Sona offers to record a note when a call ends; iOS gives no app access to ordinary call audio.

Local runs keep captured audio on the device. Cloud transcription sends audio only after you configure a provider key and accept that provider's transfer notice. Sona does not store provider keys in its settings file.

### Native macOS and phone additions

- dictation adds cleanup levels, quiet-speech capture, language choices, usage statistics, a movable bar, and answers in chat. the plain-text scratchpad saves drafts and keeps version history.
- meetings add live notes and cited help, custom notes templates, preparation briefs, and follow-up tools. email context is opt-in and stays with local generation. public web research needs a brave search key.
- folders organize meetings and limit chat to their contents. csv export covers the retained library. screen snapshots stay local; sona reads their words on this mac for search, and turning that off forgets them. recording and transcript retention preserves notes and starts with a seven-day cooldown.
- notes can be copied as formatted text or markdown. encrypted browser links require the companion service. named-viewer access, comments, and collaborative editing are not built. external destinations require their own setup and consent.
- the iphone app adds keyboard recording sessions, shortcuts, language choices, saved meeting notes, and playback. sync requires a paired companion service. optional recorded outgoing calls require a configured calling service; ordinary iphone call audio is not accessible.
- Optional dictation cleanup has a deadline on both desktop and iPhone and stops when a recording is cancelled. The iPhone saves a recoverable draft before optional history work, so slow history cannot hold the text or its audio hostage.

opening several .sona meeting files from finder now imports each one in order.
if a file fails, sona names it and continues with the others.

meeting sync saves an encrypted copy before uploading. if an upload is interrupted, sona uses that saved copy to try again.

## Install

Build the native apps from source using the commands below. A `vX.Y.Z` tag builds the native Mac app and attaches an explicitly unsigned Apple silicon ZIP to a draft release. That artifact includes the recording engine, but it is not a signed or notarized public distribution: Gatekeeper may block it, and the camera extension requires matching Apple signing and provisioning. iPhone archive and App Store export remain manual. See [BUILD.md](BUILD.md) for signing requirements.

## First launch and migration

Sona uses its own application identity and data directory. On the first launch it can move data from the **Legacy app**.

- Settings and history are copied, so the original mutable data remains available for rollback.
- Recordings and models move to avoid duplicating large files.
- Provider keys move through the operating system credential store using write, read-back, and delete steps. If the system denies access to an old key, Sona starts normally and asks you to enter that key again.
- Close the Legacy app before moving data. Running both apps can make them compete for global shortcuts.
- New setup starts with the dictation language, offers compatible speech models, and requires a real practice transcript before setup can finish.

After a successful move, remove the Legacy app using the normal macOS method. Sona never uninstalls another app. A debug-only rollback removes Sona's copied settings and history, moves recordings and models back, and discards any changes made in Sona after migration.

## Command line

Open `Sona.app` for the desktop interface. Its bundled Rust engine also provides the CLI; it does not launch a second desktop app:

```bash
core=/Applications/Sona.app/Contents/Helpers/SonaCore.app/Contents/MacOS/sona-core
"$core" --help
"$core" --list-models --json
"$core" --list-devices
"$core" --transcribe-file recording.wav --repeat 3 --json
"$core" --transcribe-file recording.wav --diagnose
"$core" --meetings --last 5
"$core" --loops --status open

"$core" --toggle-transcription
"$core" --toggle-post-process
"$core" --cancel
```

The last three controls require the native Mac app to be running. External corpus reads require Settings > Agents > External access; closing a loop also requires External mutations. Both grants start off. The MCP server wraps these same commands. See `skills/sona/SKILL.md` for flags, responses, and consent refusals.

## Data locations

The Mac engine keeps its data in `~/Library/Application Support/com.aktanazat.sona`. The native interface keeps its existing `com.aktanazat.sona.mac` identity; changing the interface does not move the engine's data.

Sona stores settings in `settings_store.json`, history in `history.db`, recordings in `recordings/`, models in `models/`, and logs in `logs/sona.log`. iPhone recordings and drafts stay in the app's private container until an approved transfer or paired sync.

### Portable core runs

For an isolated core run, create a file named `portable` next to the core executable. Sona writes `Sona Portable Mode` to that file and keeps its data in the adjacent `Data/` directory. Debug cores also need `SONA_ALLOW_PORTABLE_DEV=1`. This is not another desktop app or a public portable installer.

Portable mode does not use the native credential store, so provider-backed features remain unavailable until you use a normal installation.

## Environment variables

| Variable                             | Purpose                                                            |
| ------------------------------------ | ------------------------------------------------------------------ |
| `SONA_CORE`                          | Use a development core when the Debug Mac app has no bundled core. |
| `SONA_CORE_TARGET_DIR`               | Point Xcode at this checkout's built core/helper output root.      |
| `SONA_ALLOW_PORTABLE_DEV=1`          | Allow an isolated portable profile for a debug core.               |
| `SONA_METAL_RESIDENCY=1`             | Restore the upstream Metal residency setting.                      |
| `SONA_DEBUG_MIC_READY_DELAY_MS`      | Add a debug microphone-ready delay.                                |
| `SONA_FORCE_TRANSCRIPTION_FAILURE=1` | Force a debug transcription failure.                               |
| `SONA_FORCE_AI_STUB=1`               | Build the Apple Intelligence unavailable bridge.                   |

## Build from source

On an Apple silicon Mac, install Xcode 26, current Rust, Bun, and CMake. From the repository root, build the private helpers, the Rust engine, and then the native app:

```bash
bun install --frozen-lockfile
unset CARGO_TARGET_DIR
bun run prepare:agent-hook -- aarch64-apple-darwin
bun run prepare:cleanup-runtime -- aarch64-apple-darwin
bun run build
xcodebuild -project macos/Sona.xcodeproj -scheme Sona \
  -configuration Release -destination 'generic/platform=macOS' \
  -derivedDataPath /private/var/tmp/sona-native-release \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
  SONA_CORE_TARGET_DIR="$PWD/src-tauri/target" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build
bash macos/scripts/inspect-package.sh app \
  /private/var/tmp/sona-native-release/Build/Products/Release/Sona.app arm64
```

This produces an unsigned development bundle. Release fails if its matching release engine, either helper, or required resources are missing. A local signed build needs the existing Apple team's credentials and profiles; do not replace your installed signed app with the unsigned output.

Build the iPhone simulator app, including its Watch companion and keyboard, with:

```bash
xcodebuild -project mobile/Sona.xcodeproj -scheme Sona \
  -configuration Debug -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /private/var/tmp/sona-iphone \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build
```

[BUILD.md](BUILD.md) covers debug cores, signed Mac archives, manual iPhone export, package layout, and checks. The Rust engine and encrypted companion service remain; no Next development server or Tauri web-app build is needed.

## Troubleshooting

### macOS permissions

Sona asks for Microphone and Accessibility access during setup. Screen recording needs no Screen Recording access, because the macOS picker grants what you pick for that recording; it asks for Camera and Microphone access only when you turn those on, and its permission step opens the matching System Settings pane.

An upgrade keeps the installed app's identity, permissions, and data. Do not reset permission grants when replacing the app. Use the settings screens to inspect permission state and the application-data location.

## Licenses

The bundled **Open-source licenses** action opens the packaged `LICENSE` and `NOTICE` files.
