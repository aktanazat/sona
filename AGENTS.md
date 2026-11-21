# AGENTS.md

This file provides guidance to AI coding assistants working with code in this repository.

## Development Commands

**Prerequisites:** current stable Rust, Bun, CMake, and Xcode 26 with the macOS,
iOS and watchOS SDKs. [BUILD.md](BUILD.md) has the full list.

**Core commands:**

```bash
bun install --frozen-lockfile
bun run build:debug       # debug engine, src-tauri/target/debug/sona
bun run build             # release engine, src-tauri/target/release/sona
```

The Mac app (`macos/Sona.xcodeproj`) and the iPhone app (`mobile/Sona.xcodeproj`)
build with `xcodebuild`. [BUILD.md](BUILD.md) has the exact commands, including
the two helpers the Mac app bundles. There is no web dev server.

**Formatting and linting (run before committing):**

```bash
bun run format            # Prettier + cargo fmt
bun run format:check      # Check formatting without changes
bun run format:backend    # cargo fmt only
bun run lint:backend      # cargo clippy
bun run lint:anti-slop    # oxlint against tools/oxlint/baseline.json
```

**Verifying a change against a running Sona:**

Never verify against the installed app or a `cargo run` that uses the default
data directory: both read and write the developer's own settings store, so a
verification step lands on a daily preference. Microphone detection is the
sharp case — flipping it to watch a prompt leaves the prompt answered and the
preference moved. Give the run its own profile instead:

```bash
PROFILE=/private/var/tmp/sona-verify
mkdir -p "$PROFILE" && cp src-tauri/target/debug/sona "$PROFILE/"
printf 'Sona Portable Mode' > "$PROFILE/portable"
cd "$PROFILE" && SONA_ALLOW_PORTABLE_DEV=1 ./sona --loops
```

`/private/var/tmp` rather than `/tmp`: a profile is worth keeping between
commands and across a reboot, and macOS clears `/tmp` on boot.

Portable mode moves the settings store, logs, models and recordings under the
adjacent `Data/` directory, so seeding a preference there exercises the real
code against a throwaway state. `SONA_ALLOW_PORTABLE_DEV=1` is required for a
debug build, since a marker in `target/debug` is usually a mistake. Native
credential storage stays off in portable mode, so keychain-backed features
(the encrypted corpus) are not reachable this way — verify those against a
profile you own and restore what you changed.

A portable GUI is safe to launch beside the installed app: on macOS it stays
off the bundle-wide single-instance socket and holds an OS lock on its own
`Data/` directory, so it cannot be mistaken for a second copy of the install
and cannot be handed the install's forwarded arguments.

**Building and testing the Rust side:**

```bash
bun run test:backend      # cargo test --lib, in a target dir this checkout owns
bun run build:debug       # same, for the engine
bun run build
```

Go through these rather than calling `cargo` directly, and a second checkout —
a worktree for a comparison build, say — stays out of this one's way. Two
checkouts pointed at one `CARGO_TARGET_DIR` resolve to the same cargo unit,
and cargo decides that unit is fresh by comparing mtimes, so whichever tree
has the older files silently inherits the other's compiled artifacts: Rust and
the Swift bridges both. `scripts/cargo-target-dir.ts` keeps that from
happening, and `scripts/prepare-agent-hook.ts` builds and stages the sidecar
in the same directory it resolves, so the Mac app never bundles a copy
from somewhere else. With `CARGO_TARGET_DIR` unset none of this changes
anything, because cargo's default `src-tauri/target` is already
per-checkout; with one set — a cache root on a faster disk is the reason to —
each checkout builds in its own directory beneath it.

For detailed platform-specific build setup, see [BUILD.md](BUILD.md).

## Architecture Overview

Sona is a native SwiftUI Mac app (`macos/Sona/`) and iPhone app (`mobile/Sona/`) for dictation and meeting notes. The Mac app starts the Rust engine in `src-tauri/` as `sona-core --native-socket <path>` and talks to it over that Unix socket, one JSON object per line (`native_bridge/`). Tauri remains an engine dependency; it no longer supplies a web interface.

### Backend Structure (src-tauri/src/)

- `lib.rs` - Main entry point, Tauri setup, manager initialization
- `managers/` - Core business logic:
  - `audio.rs` - Audio recording and device management
  - `model.rs` - Model downloading and management
  - `transcription.rs` - Speech-to-text processing pipeline
  - `history.rs` - Transcription history storage
- `audio_toolkit/` - Low-level audio processing:
  - `audio/` - Device enumeration, recording, resampling
  - `vad/` - Voice Activity Detection (Silero VAD)
- `commands/` - Tauri command handlers, called by the Mac app through `native_bridge/`.
  After adding, removing or re-signing a command, run `bun run generate:native-dispatch`
  to regenerate `native_bridge/dispatch.rs`; without it the Mac app gets
  `unknown method`. Params keep the argument names the old webview bindings used.
- `cli.rs` - CLI argument definitions (clap derive)
- `shortcut/` - Global keyboard shortcut handling
- `settings.rs` - Application settings management
- `overlay.rs` - Recording overlay window (platform-specific)
- `signal_handle.rs` - `send_transcription_input()` reusable function
- `utils.rs` - Platform detection helpers

### Native apps

- `macos/Sona/` - SwiftUI Mac interface; `Core.swift` starts the engine and owns its socket
- `macos/SonaCamera/` - Camera extension
- `mobile/Sona/` - iPhone app; `mobile/SonaWatch/`, `mobile/SonaKeyboard/` and
  `mobile/SonaWidgets/` ship inside its package
- `cloudflare/sona-companion/` - Paired sync service
- `tools/sona-mcp/` - MCP server over the engine's command line

### Key Architecture Patterns

**Manager Pattern:** Core functionality organized into managers (Audio, Model, Transcription) initialized at startup and managed via Tauri state.

**Command-Event Architecture:** Mac app → engine via Tauri command names sent over the native socket; engine → Mac app via events on the same socket.

**Pipeline Processing:** Audio → VAD → Whisper/Parakeet → Text output → Clipboard/Paste

**State Flow:** Mac app → native socket → Tauri command → Rust State → Persistence (tauri-plugin-store)

### Technology Stack

**Core Libraries:**

- `transcribe-cpp` - Local Whisper-family inference (GGML/GGUF) with GPU acceleration
- `transcribe-rs` - ONNX speech recognition (Parakeet, Moonshine, SenseVoice, etc.)
- `cpal` - Cross-platform audio I/O
- `vad-rs` - Voice Activity Detection
- `rdev` - Global keyboard shortcuts
- `rubato` - Audio resampling
- `rodio` - Audio playback for feedback sounds

### Application Flow

1. **Initialization:** The Mac app starts the engine with `--native-socket <path> --no-tray --start-hidden`; the engine loads settings and initializes managers
2. **Model Setup:** First-run downloads preferred Whisper model (Small/Medium/Turbo/Large)
3. **Recording:** Global shortcut triggers audio recording with VAD filtering
4. **Processing:** Audio sent to Whisper model for transcription
5. **Output:** Text pasted to active application via system clipboard

### Settings System

Settings are stored using Tauri's store plugin with reactive updates:

- Keyboard shortcuts (configurable, supports push-to-talk)
- Audio devices (microphone/output selection)
- Model preferences (Small/Medium/Turbo/Large Whisper variants)
- Audio feedback and translation options

### Single Instance Architecture

The engine enforces single instance behavior. Remote control flags (`--toggle-transcription`, etc.) work by launching a second instance that sends args to the running instance via `tauri_plugin_single_instance`, then exits.

## Localization

The Mac interface has English copy only. The iPhone and Apple Watch apps use
native strings files: `mobile/Sona/en.lproj/Localizable.strings` and
`mobile/SonaWatch/en.lproj/Localizable.strings`. See
[CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md).

## Code Style

**Rust:**

- Run `bun run format:backend` and `bun run lint:backend` before committing —
  `cargo fmt` and `cargo clippy` in a target directory this checkout owns
- Handle errors explicitly (avoid unwrap in production)
- Use descriptive names

**SwiftUI:**

- Use native controls and the existing Mac or phone theme
- Check light and dark appearance and larger text sizes

**TypeScript (scripts, `tools/sona-mcp/`, `cloudflare/sona-companion/`):**

- Strict TypeScript, avoid `any` types

## CLI Parameters

The engine (`sona-core`) takes command-line parameters for scripts and automation. Run directly, it accepts only headless, query and remote-control flags; anything else exits 2 and points to the native app (`lib.rs`). The Mac app owns the window and the menu bar item.

**Implementation:** `cli.rs` (definitions), `main.rs` (parsing), `lib.rs` (applying), `signal_handle.rs` (shared logic)

| Flag                     | Description                                            |
| ------------------------ | ------------------------------------------------------ |
| `--toggle-transcription` | Toggle recording on/off on a running instance          |
| `--toggle-post-process`  | Toggle recording with post-processing on/off           |
| `--cancel`               | Cancel the current operation on a running instance     |
| `--start-hidden`         | Start with no window; the Mac app passes it            |
| `--no-tray`              | Start without the engine's tray; the Mac app passes it |
| `--debug`                | Enable debug mode with verbose (Trace) logging         |

**Headless corpus verbs (D15):** one JSON value on stdout, one JSON refusal on
stderr, and nothing else on either. Every verb requires
`Settings > Agents > External access`, which is off on install; eight only
read, and `--loop-resolve` reads its loop revision before writing. That one
write also needs `Settings > Agents > External mutations`, a second row also
off on install. With either needed grant off, it refuses with
`{"error":"consent_required","settings_path":…}`.

| Flag                                                                  | Description                                                       |
| --------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `--query <TEXT> [--scope S] [--limit N]`                              | Search the corpus. `S` ∈ all\|meetings\|dictations\|people\|loops |
| `--meetings [--last N \| --from D --to D]`                            | Retained meetings, newest first. Dates are local `YYYY-MM-DD`     |
| `--meeting <ID>`                                                      | One meeting: summary, headline, notes, ledger rows                |
| `--transcript <ID>`                                                   | One meeting's speaker-labeled transcript                          |
| `--loops [--status open\|done] [--mine\|--waiting] [--after LOOP_ID]` | Loops and commitments across the corpus                           |
| `--people <NAME>`                                                     | Look a person up by name, alias or calendar address               |
| `--events [--after <ID>]`                                             | Receipts and workflow runs, newest first                          |
| `--upcoming [--limit N]`                                              | Today plus the next seven local days of calendar                  |
| `--plans [--limit N]`                                                 | Plans to meet heard in meetings, with quote and status            |
| `--bookmarks [--limit N]`                                             | Bookmarks pressed while recording, newest meeting first           |
| `--roster <ID>`                                                       | The names one meeting's call listed                               |
| `--place <ID> [--limit N]`                                            | Where one meeting was recorded and recordings within 100 m        |
| `--loop-resolve <LOOP_ID>`                                            | **Writes.** Marks one loop done, prints the `OperationReceipt`    |

Exit codes: 0 answered, 2 bad input (`invalid_request`), 1 everything else
(`consent_required`, `unavailable`, `not_found`, `failed`). The plane lives in
`query/external.rs`; `tools/sona-mcp/` is a thin MCP server over exactly these
flags, and `skills/sona/SKILL.md` is the agent-facing reference for both.

**Tracker verbs:** `sona work <verb>` reads and changes tasks, printing text,
or one JSON value with `--json`. Tasks are named `S-12`, `12` or by id. Reads
and every `--dry-run` need External access; every other verb, `export`
included, also needs External mutations. Writes are filed as an agent's,
named by `--agent NAME` (the MCP server passes its client's name). Without
`--operation-id` a write derives its id from the request and what it read, so
a repeated command never changes anything twice. Unless `Settings > Agents >
Agents' changes` is Apply, an agent's field changes wait as proposals the user
accepts or rejects in the app (`planning/work/agents.rs`). The verbs live in
`query/external/tracker.rs` over `planning::work`; `sona work --help` lists
them. `list` and `ready` take `--due overdue,today,this-week,next:N,none`;
`create` and `update` take `--repeat "every monday"` (`none` stops it).

| Verb                                   | Does                                                             |
| -------------------------------------- | ---------------------------------------------------------------- |
| `list`, `ready`, `mine`, `counts`      | Tasks most urgent first; unblocked Todo work; your work; numbers |
| `show <TASK>`                          | One task with its quote, links, evidence and history             |
| `create`, `update`                     | **Writes.** `update --dry-run` shows each field's from and to    |
| `claim <TASK>`                         | **Writes.** Todo or Backlog to In progress with the agent on it  |
| `done <TASK>... [--evidence URL]`      | **Writes.** Closes its commitment; keeps a URL as evidence       |
| `accept`, `decline`, `duplicate`       | **Writes.** Triage decisions                                     |
| `archive`, `restore`                   | **Writes.** Hide tasks from every list, or bring them back       |
| `snooze <TASK>... --until DAY`         | **Writes.** Off lists and `ready` until DAY; `none` wakes it     |
| `relate`, `unrelate`                   | **Writes.** `S-1 blocks S-2`, `blocked-by` or `related`          |
| `labels`, `label-save`, `label-delete` | Labels; the saves and deletes write                              |
| `views`, `view-save`, `view-delete`    | Saved views; the saves and deletes write                         |
| `export --md <DIR>`                    | **Writes.** One `S-<n>.md` file per matching task                |
| `ask <QUESTION> [--take N,...]`        | Cited lines answering it; `--take` writes them as Triage tasks   |
| `say <WORDS>`                          | **Writes.** "push everything not urgent to monday"; `--dry-run`  |
| `watches`, `watch`, `unwatch`          | Topics Sona notifies about; watch and unwatch write              |

In a portable profile only the tracker's history side opens: work from
meetings, people as owners and `--meeting` filters need the encrypted corpus.

**Shared page verbs:** one meeting's notes as an encrypted page on the user's site
(`shared_pages/`, `query/external/share.rs`). Text by default, one JSON value
with `--json`, refusals as JSON on stderr. Reads and every `--dry-run` need
External access; `--save`, `share --confirm`, `unshare` and setting
`share-site` also need External mutations. `share` publishes only with
`--confirm` run at a terminal, after it prints the page's title, sections,
expiry and site on that terminal and the user types `share` and presses
Return. Without `--confirm`, or with any other answer, the refusal is
`confirmation_required`; from a process with no terminal on stdin it is
`confirmation_requires_terminal` (both exit 2, nothing pushed). Agents
prepare drafts; the owner shares them from Sona. Meetings kept local are
`not_found` on every route, a `--draft FILE` and the app's commands
included; when meetings cannot be opened outside a portable profile, the
refusal is `unavailable`. Unsharing a `failed` share still pushes its removal.

| Verb                                                                                   | Does                                                                     |
| -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `share-preview <MEETING> [--include S,...] [--speakers names\|numbered] [--expires 1d\|7d\|30d\|never]` | The exact page; `--save [--agent NAME]` keeps it as a draft (**writes**); `--html FILE` |
| `share <MEETING> [--draft FILE] [same choices] --dry-run`                              | Page counts and the files a push would write; publishes nothing          |
| `share <MEETING> [--draft FILE] [same choices] --confirm`                              | **Publishes** once `share` is typed back, then waits up to 4 minutes for the page to be live |
| `shares`                                                                               | Every share with its state; pending ones are checked once                |
| `unshare <SLUG\|URL> [--dry-run]`                                                      | **Writes.** Removes the page and its files from the site                 |
| `share-site [--remote URL --base-url URL]`                                             | Where pages are pushed and served; setting it **writes**                 |

Sections for `--include`: `title`, `summary`, `action_items`, `decisions`,
`notes`, `transcript`, `audio`. Further refusals: `site_not_set_up`,
`too_large`, `audio_unavailable`, `push_failed`, `locked`,
`remote_not_sona_site`. The app reaches the same core through the native
commands `shared_draft`, `shared_draft_save`, `shared_draft_discard`,
`shared_drafts`, `shared_preview`, `shared_publish`, `shared_list`,
`shared_revoke`, `shared_site_get` and `shared_site_set`.

**Key design decisions:**

- CLI flags are runtime-only overrides — they do NOT modify persisted settings
- Remote control flags work via `tauri_plugin_single_instance`: second instance sends args, then exits
- `send_transcription_input()` in `signal_handle.rs` is shared between signal handlers and CLI

## Debug Mode

Access debug features: Settings → Debug in the Mac app.

## Platform Notes

- **macOS**: Metal acceleration, accessibility permissions required for keyboard shortcuts

## Troubleshooting

See the [Troubleshooting](README.md#troubleshooting) section in README.md.

## GitHub workflow for AI coding assistants

PRs go to `aktanazat/sona` with `gh pr create --repo aktanazat/sona` and follow [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md).

- **Translations:** Follow [CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md).
- **Full contributor workflow:** [CONTRIBUTING.md](CONTRIBUTING.md).
