# Building Sona

On macOS, Sona uses the native SwiftUI app in `macos/` with a bundled Rust core. The Tauri 2 app and Bun-managed React frontend remain the desktop surface on Windows and Linux and the frontend development surface.

## Prerequisites

- Current stable Rust with the platform toolchain.
- Bun.
- CMake, a C++17 compiler, and the platform libraries required by Tauri and the selected transcription runtime.

```bash
bun install
bun run prepare:agent-hook -- aarch64-apple-darwin
bun run prepare:cleanup-runtime -- aarch64-apple-darwin
```

Use your own Cargo target triple in place of `aarch64-apple-darwin`. `bun run tauri build` stages both helpers through `beforeBuildCommand`. Fresh Rust builds need the `sona-agent-hook` and `sona-cleanup` sidecars staged once because `src-tauri/build.rs` reads `bundle.externalBin` from `tauri.conf.json`. The cleanup build also stages the llama.cpp, JSON library, and model licenses in `resources/cleanup-runtime/`; the model itself is downloaded only when selected in the app.

## Development

```bash
bun run tauri dev
```

If the macOS CMake policy setting is required by a local toolchain:

```bash
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev
```

The frontend-only commands are:

```bash
bun run dev
bun run build
bun run preview
```

## Checks

keep tests for observable behavior, data integrity, privacy, public contracts and measured resource limits. remove tests that only pin wording, code layout, private bookkeeping or an answer supplied by the test itself. a meaningful failing test stays.

```bash
bun run build
bun run lint
bun run lint:anti-slop
bun run check:translations
bun run check:onboarding
bun test
cd cloudflare/sona-companion && bun run typecheck && bun run test
cd src-tauri && bun ../scripts/cargo-target-dir.ts cargo check --all-features
cd src-tauri && bun ../scripts/cargo-target-dir.ts cargo test --lib
cd src-tauri && bun ../scripts/cargo-target-dir.ts cargo test --bin sona-agent-hook
```

`scripts/cargo-target-dir.ts` gives this checkout its own target directory
beneath a `CARGO_TARGET_DIR` cache root, so a second checkout building at the
same time cannot hand this one its artifacts. With `CARGO_TARGET_DIR` unset it
runs the command unchanged, and plain `cargo` is equivalent.

The package audit expects executable `sona-agent-hook` and `sona-cleanup` sidecars and the `sona` core. The native macOS build copies and signs `sona-cleanup` beside `sona-core` inside `Contents/Helpers/SonaCore.app`.

the active local meeting acceptance is `meeting::processing::real_engine_acceptance::tests::actual_apple_long_meeting_pipeline_and_regeneration`. it requires a ready apple intelligence model and uses the real imported-meeting pipeline, not a model substitute. it checks the original long conversation, then checks regenerated notes against the same decisions, owners, revised deadlines and citations. requests and results are saved under `.progress/meeting-acceptance/`; the original request and reply-token limits remain enforced at the sdk boundary. a cloud-only pass does not replace this check.

the native ledger presents each literal excerpt with its original source reference and speaker. a speaker is not automatically a task owner, and the person who settles a choice never owns it: a speaker's own undertaking is a personal promise. a promise version whose deadline is not in its own cited excerpts, or whose excerpts do not exist, is dropped. when a settled choice and a promise select exactly the same excerpts, the app keeps one role: the promise when the choice names the same person, the choice otherwise. stances use the same bounded excerpt receipts as other ledger facts, and the app restores all original source references before canonical validation. the separate real-model named-assignment check is `actual_apple_named_assignment_is_not_the_speaker`; run it explicitly with `--ignored`. passing that check does not replace the complete initial and regenerated acceptance.

a long meeting's ledger is read in parts and the parts are joined by the app, not re-read by the model. threads, commitments, stances and open questions keep the roles their part gave them; identical subjects with the same state and owner become one thread with every source reference. one small model request writes the headline and may name promises that are versions of the same work by the same person, a later closed thread that cancels a promise, or a later thread that answers an open question. the app accepts a cancellation or an answer only from a thread whose source comes after the promise or question. these checks do not establish real-model accuracy.

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib --features cloud-realtime actual_apple_long_meeting_pipeline_and_regeneration -- --nocapture --test-threads=1
```

## Version bump

1. `src-tauri/Cargo.toml` `[package] version` — authoritative for the Rust core and Tauri app.
2. `src-tauri/Cargo.lock` — run any cargo command, then commit the changed `sona` entry.
3. `package.json` `version` and `src/content/release-notes/index.ts` — match the core version and add its release notes.
4. `macos/Sona.xcodeproj/project.pbxproj` — match `MARKETING_VERSION` and increase `CURRENT_PROJECT_VERSION` for the native app and its extension.
5. `mobile/Sona.xcodeproj/project.pbxproj` — update `MARKETING_VERSION` and `CURRENT_PROJECT_VERSION` only when shipping the companion app.

`src-tauri/tauri.conf.json` deliberately carries no `version` field. With it absent, `tauri-codegen` compiles `PackageInfo.version` from `env!("CARGO_PKG_VERSION")`, so the core's About screen, What's New, `sona --version`, and the Tauri bundle agree. The native macOS shell and its helper bundle use the Xcode marketing version from step 4. `tools/sona-mcp` and `cloudflare/sona-companion` version independently. The following check prints one line when the core, frontend, and latest release notes agree, and two if a version reappears in the Tauri config:

```bash
{ awk -F'"' '/^version = /{print $2; exit}' src-tauri/Cargo.toml; jq -r '.version // empty' src-tauri/tauri.conf.json; jq -r .version package.json; sed -n 's/^  "\([0-9][0-9.]*\)".*/\1/p' src/content/release-notes/index.ts | sort -V | tail -1; } | sort -u
```

## Package layout

| Platform | Main executable                | Application identity | Private runtime directory |
| -------- | ------------------------------ | -------------------- | ------------------------- |
| macOS    | `Sona.app/Contents/MacOS/Sona`, with `sona-core` in `Contents/Helpers/SonaCore.app` | `com.aktanazat.sona.mac`, `com.aktanazat.sona.core` | Inside the app bundle |
| Windows  | `sona.exe`                     | `com.aktanazat.sona` | Next to the executable    |
| Linux    | `sona`                         | `com.aktanazat.sona` | `/usr/lib/sona`           |

The native macOS app and camera extension require a development certificate and matching provisioning profile for team `AAVB324H37`, including the shared app group and System Extension capability. Keep the installed app's signing identity when replacing its complete bundle. A development signature is not notarization or a distribution identity; this repository does not provide distribution credentials or automatic updates.

## Platform notes

### macOS

The native SwiftUI app requires macOS 15 or later. The Tauri core retains its macOS 10.15 deployment target. Its optional downloadable cleanup helper requires macOS 11 or later and runs on the CPU without an external server. The app checks helper availability before offering offline cleanup.

Apple Intelligence requires macOS 26 or later and a ready system model. The build uses FoundationModels when the active toolchain supplies it; otherwise it uses the unavailable bridge. Set `SONA_FORCE_AI_STUB=1` to force that bridge.

For a native release, build the Rust core in release mode, then build `macos/Sona.xcodeproj` with `SONA_CORE_TARGET_DIR` pointing to that target's output directory. The bundle phase prefers `release/sona`, includes its resources and both helpers, and signs the private helper bundle. Replace `/Applications/Sona.app` as a whole bundle; do not overwrite its signed executable in place or leave another runnable Sona app registered.

Use the app's permission screen for Microphone, Accessibility, Screen Recording, and Camera access as needed. An upgrade must preserve existing grants and data; do not reset permissions as part of installation.

### Windows

The NSIS installer uses the Sona product name and installs under the selected Sona directory. CI sets `SONA_VC_REDIST_DIRS` when it stages the app-local VC++ runtime.

### Linux

The deb and rpm packages install Sona's private native libraries in `/usr/lib/sona`; the executable's rpath points there. The AppImage continues to use its own `usr/lib` layout.

If a Wayland compositor has trouble with the recording overlay, run:

```bash
SONA_NO_GTK_LAYER_SHELL=1 sona
```

## Portable installations

Place a `portable` marker beside the executable. Sona recognizes `Sona Portable Mode`, writes that marker for new portable installs, and keeps data under the adjacent `Data/` directory. It accepts the older marker only to preserve an in-place upgrade.

Portable mode intentionally disables native credential storage. Cloud and provider-key features stay unavailable there.

## Migrating from the Legacy app

On first launch Sona offers to move settings, history, recordings, models, and configured provider keys from the Legacy app. Close the Legacy app first. Sona copies mutable data, moves large blobs, journals each operation, and writes a completion receipt only after migration finishes.

If migration needs to be reversed, use the debug rollback action while the Legacy app is closed. The rollback discards Sona-era settings and history changes, then moves recordings and models back. It does not uninstall the Legacy app or re-register its macOS login item.
