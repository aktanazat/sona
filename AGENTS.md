# AGENTS.md

This file provides guidance to AI coding assistants working with code in this repository.

## Development Commands

**Prerequisites:**

- [Rust](https://rustup.rs/) (latest stable)
- [Bun](https://bun.sh/) package manager

**Core Development:**

```bash
# Install dependencies
bun install

# Run in development mode
bun run tauri dev
# If cmake error on macOS:
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev

# Build for production
bun run tauri build

# Frontend only development
bun run dev        # Start the Next dev server on :1420 (the port tauri expects)
bun run build      # Typecheck + static export of the three window routes to out/
bun run preview    # Serve the exported out/ directory
```

**Linting and Formatting (run before committing):**

```bash
bun run lint              # ESLint for frontend
bun run lint:fix          # ESLint with auto-fix
bun run format            # Prettier + cargo fmt
bun run format:check      # Check formatting without changes
bun run format:frontend   # Prettier only
bun run format:backend    # cargo fmt only
```

**Model Setup (Required for Development):**

```bash
mkdir -p src-tauri/resources/models
curl -o src-tauri/resources/models/silero_vad_v4.onnx https://blob.handy.computer/silero_vad_v4.onnx
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
bun run tauri dev         # same, for the app
bun run tauri build
```

Go through these rather than calling `cargo` directly, and a second checkout —
a worktree for a comparison build, say — stays out of this one's way. Two
checkouts pointed at one `CARGO_TARGET_DIR` resolve to the same cargo unit,
and cargo decides that unit is fresh by comparing mtimes, so whichever tree
has the older files silently inherits the other's compiled artifacts: Rust and
the Swift bridges both. `scripts/cargo-target-dir.ts` keeps that from
happening, and `scripts/prepare-agent-hook.ts` builds and stages the sidecar
in the same directory it resolves, so a `tauri build` never bundles a copy
from somewhere else. With `CARGO_TARGET_DIR` unset none of this changes
anything, because cargo's default `src-tauri/target` is already
per-checkout; with one set — a cache root on a faster disk is the reason to —
each checkout builds in its own directory beneath it.

For detailed platform-specific build setup, see [BUILD.md](BUILD.md).

## Architecture Overview

Sona is a cross-platform desktop speech-to-text application built with Tauri 2.x (Rust backend + React/TypeScript frontend).

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
- `commands/` - Tauri command handlers for frontend communication
- `cli.rs` - CLI argument definitions (clap derive)
- `shortcut.rs` - Global keyboard shortcut handling
- `settings.rs` - Application settings management
- `overlay.rs` - Recording overlay window (platform-specific)
- `signal_handle.rs` - `send_transcription_input()` reusable function
- `utils.rs` - Platform detection helpers

### Frontend Structure (src/)

- `App.tsx` - Main component with onboarding flow
- `components/` - React UI components:
  - `vg/` - **the primitive kit, and the only place primitives live.** Every
    button, input, select, dialog, popover, menu, switch, slider, tab and
    tooltip comes from here: shadcn/ui components, Geist-coloured through the
    `@theme inline` token bridge in `app/globals.css`. `components.json` points
    the shadcn CLI at it (`"ui": "@/components/vg"`), so `bunx shadcn add …`
    lands here. Never hand-roll a control, and never start a second kit.
  - `vg/chart/` - deterministic inline SVG primitives. Import `Bars`,
    `Sparkline`, and `Ring` from `@/components/vg/chart`; feature code supplies
    translated aria sentences.
  - `charts/` - app-level chart composition. Import `ChartCard` from
    `@/components/charts`; it may compose settings rows and vg primitives.
  - `audio/AudioPlayer.tsx`, `RouteSkeleton.tsx`, `Toaster.tsx` - app
    components, not primitives: the transcript scrubber, the shape of a
    settings page before its chunk arrives, and the app's one toast root. Each
    owns behaviour specific to this app, which is why it sits outside `vg/` —
    `vg/` is the only primitives home, and the pre-Geist `ui/` kit it replaced
    is gone.
  - `settings/` - Settings UI. `settings/rows.tsx` is its composition layer
    (SettingsPage, SettingsCard, SettingsSurface, SettingsRow, Notice,
    `PAGE_COLUMN`); pages compose those, they never restate the surface
    literals.
  - `model-selector/` - Model management interface
  - `onboarding/` - First-run experience
  - `icons/`, `analytics/`, `cloud-sync/`, `whats-new/` - feature-local components
- `hooks/useSettings.ts` - Settings state management hook
- `stores/settingsStore.ts` - Zustand store for settings
- `bindings.ts` - Auto-generated Tauri type bindings (via tauri-specta)
- `overlay/` - Recording overlay window entry point
- `lib/types.ts` - Shared TypeScript type definitions

### Key Architecture Patterns

**Manager Pattern:** Core functionality organized into managers (Audio, Model, Transcription) initialized at startup and managed via Tauri state.

**Command-Event Architecture:** Frontend → Backend via Tauri commands; Backend → Frontend via events.

**Pipeline Processing:** Audio → VAD → Whisper/Parakeet → Text output → Clipboard/Paste

**State Flow:** Zustand → Tauri Command → Rust State → Persistence (tauri-plugin-store)

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

1. **Initialization:** App starts minimized to tray, loads settings, initializes managers
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

The app enforces single instance behavior — launching when already running brings the settings window to front rather than creating a new process. Remote control flags (`--toggle-transcription`, etc.) work by launching a second instance that sends args to the running instance via `tauri_plugin_single_instance`, then exits.

## Internationalization (i18n)

All user-facing strings must use i18next translations. ESLint enforces this (no hardcoded strings in JSX).

**Adding new text:**

1. Add key to `src/i18n/locales/en/translation.json`
2. Use in component: `const { t } = useTranslation(); t('key.path')`

**File structure:**

```
src/i18n/
├── index.ts           # i18n setup
├── languages.ts       # Language metadata
└── locales/
    ├── en/translation.json  # English (source)
    ├── de/, es/, fr/, ja/, ru/, zh/, ...
    └── ...
```

For translation contribution guidelines, see [CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md).

## Code Style

**Rust:**

- Run `bun run format:backend` and `bun run lint:backend` before committing —
  `cargo fmt` and `cargo clippy` in a target directory this checkout owns
- Handle errors explicitly (avoid unwrap in production)
- Use descriptive names, add doc comments for public APIs

**TypeScript/React:**

- Strict TypeScript, avoid `any` types
- Functional components with hooks
- Tailwind CSS for styling
- Path aliases: `@/` → `./src/`

## CLI Parameters

Sona supports command-line parameters on all platforms for integration with scripts, window managers, and autostart configurations.

**Implementation:** `cli.rs` (definitions), `main.rs` (parsing), `lib.rs` (applying), `signal_handle.rs` (shared logic)

| Flag                     | Description                                                |
| ------------------------ | ---------------------------------------------------------- |
| `--toggle-transcription` | Toggle recording on/off on a running instance              |
| `--toggle-post-process`  | Toggle recording with post-processing on/off               |
| `--cancel`               | Cancel the current operation on a running instance         |
| `--start-hidden`         | Launch without showing the main window (tray icon visible) |
| `--no-tray`              | Launch without system tray (closing window quits the app)  |
| `--debug`                | Enable debug mode with verbose (Trace) logging             |

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
| `--loop-resolve <LOOP_ID>`                                            | **Writes.** Marks one loop done, prints the `OperationReceipt`    |

Exit codes: 0 answered, 2 bad input (`invalid_request`), 1 everything else
(`consent_required`, `unavailable`, `not_found`, `failed`). The plane lives in
`query/external.rs`; `tools/sona-mcp/` is a thin MCP server over exactly these
flags, and `skills/sona/SKILL.md` is the agent-facing reference for both.

**Key design decisions:**

- CLI flags are runtime-only overrides — they do NOT modify persisted settings
- Remote control flags work via `tauri_plugin_single_instance`: second instance sends args, then exits
- `send_transcription_input()` in `signal_handle.rs` is shared between signal handlers and CLI

## Debug Mode

Access debug features: `Cmd+Shift+D` (macOS) or `Ctrl+Shift+D` (Windows/Linux)

## Platform Notes

- **macOS**: Metal acceleration, accessibility permissions required for keyboard shortcuts
- **Windows**: Vulkan acceleration, code signing
- **Linux**: OpenBLAS + Vulkan, limited Wayland support, overlay uses GTK layer shell (disable with `HANDY_NO_GTK_LAYER_SHELL=1`)

## Troubleshooting

See the [Troubleshooting](README.md#troubleshooting) section in README.md.

## GitHub workflow for AI coding assistants

**MANDATORY. Before opening any PR, issue, or discussion in this repo: you MUST read the relevant template file and follow it strictly.** That includes sections that look "ceremonial" — checklists, AI Assistance disclosures, "Human Written Description". A generic Summary/Test-plan layout is not acceptable.

- **Opening a PR:** Read [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md). Every section listed there is mandatory. If a section requires a human-written paragraph (e.g. "Human Written Description"), leave a clear TODO placeholder and ask the human contributor to fill it in — do not invent their voice.
- **Opening an issue:** Read [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/). Blank issues are disabled; pick the right template (`bug_report.md` for bugs). Feature requests do not belong in issues — they go to [Discussions](https://github.com/aktanazat/sona/discussions) (see `.github/ISSUE_TEMPLATE/config.yml`).
- **Proposing a feature:** Sona is under a feature freeze. New features require community support gathered in [Discussions](https://github.com/aktanazat/sona/discussions) before any PR is opened — see the PR template's "Community Feedback" section.
- **Translations:** Follow [CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md).
- **Full contributor workflow:** [CONTRIBUTING.md](CONTRIBUTING.md).

**Commits:** Use conventional commit prefixes (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`). Focus the message on _why_, not _what_.

<!-- BEGIN:nextjs-agent-rules -->

# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->
