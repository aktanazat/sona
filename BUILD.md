# Building Sona

Sona has one native Mac app in `macos/` and one native iPhone app in `mobile/`. The iPhone app embeds its Watch companion and dictation keyboard. The Rust code in `src-tauri/` is the Mac recording, transcription, meeting, sync, and command-line engine; it is not a second desktop app. There is no web desktop development server or Windows/Linux release path.

## Requirements

- macOS 15 or later for the native Mac app.
- Xcode 26 with the macOS, iOS 26, and watchOS SDKs. Select it with `xcode-select` before building. The iPhone app requires iOS 26; its embedded Watch companion targets watchOS 10 or later.
- Current stable Rust, Bun, CMake, and Xcode's C/C++ tools.
- An Apple silicon Mac for the commands and CI artifacts below. CI does not produce an Intel or universal Mac bundle.
- `ffmpeg` for the Rust media-import tests, not for launching the app.

```bash
brew install cmake ffmpeg
bun install --frozen-lockfile
```

Run the commands below from the repository root. Model resources needed at startup are checked in. Speech models and the optional cleanup model are downloaded through the app when selected.

Apple Intelligence needs macOS 26 or later, supported hardware, and a ready system model. It is not required to compile the rest of Sona. `SONA_FORCE_AI_STUB=1` selects the unavailable Apple Intelligence bridge for development.

## Build the Mac app

Build both private helpers before the Rust engine, then build the native Xcode app. These commands use this checkout's default Cargo output directory and make an unsigned development bundle:

```bash
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

`bun run build` compiles `src-tauri/target/release/sona`. The agent-hook preparation command compiles the matching release helper under `src-tauri/target/aarch64-apple-darwin/release/`. The cleanup preparation command builds and stages its CPU runtime under `src-tauri/target/cleanup-runtime/aarch64-apple-darwin/stage/` and copies its dependency/model licenses into `src-tauri/resources/cleanup-runtime/`. Both preparation commands also maintain the sidecar staging files in `src-tauri/binaries/`.

The Xcode bundle phase copies those built artifacts, not an arbitrary binary from another configuration. Release refuses a missing core, either helper, or required resources. It never substitutes a debug core. The inspection command checks the app, camera extension, helper identities, architecture, matching versions, bundled notices, model metadata, and the real helpers' safe command-line responses. It does not record audio, open the app, or establish signing/notarization.

An unsigned bundle is not a public release. Gatekeeper can block it, and the camera extension needs matching Apple signing and provisioning. Do not replace an installed signed app with this development artifact.

### Debug builds

Use the matching debug core and agent hook:

```bash
unset CARGO_TARGET_DIR
bun run prepare:agent-hook -- aarch64-apple-darwin --debug
bun run prepare:cleanup-runtime -- aarch64-apple-darwin
bun run build:debug
xcodebuild -project macos/Sona.xcodeproj -scheme Sona \
  -configuration Debug -destination 'generic/platform=macOS' \
  -derivedDataPath /private/var/tmp/sona-native-debug \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
  SONA_CORE_TARGET_DIR="$PWD/src-tauri/target" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build
```

Cleanup remains release-optimized in both configurations. A Debug Xcode build with no debug core removes any previous bundled helper and warns that the app needs the runtime `SONA_CORE` override. That override is used only when no bundled core is present. It does not let a Release build omit the engine. A present core with missing helpers or resources fails in either configuration.

### Cargo caches and other checkouts

The root build/test commands use `scripts/cargo-target-dir.ts`. If `CARGO_TARGET_DIR` is set, it is a cache root; the script resolves a private child for this checkout. Helper preparation uses that same resolver. Set Xcode's `SONA_CORE_TARGET_DIR` to that resolved child, not the shared cache root. Leaving `CARGO_TARGET_DIR` unset, as above, uses `src-tauri/target` and avoids ambiguity.

## Signing and distribution

The existing native identities must stay together:

| Product                  | Bundle identifier                       |
| ------------------------ | --------------------------------------- |
| Mac app                  | `com.aktanazat.sona.mac`                |
| Private core helper      | `com.aktanazat.sona.core`               |
| Camera extension         | `com.aktanazat.sona.mac.camera`         |
| iPhone app               | `com.aktanazat.sona.mobile`             |
| Embedded Watch companion | `com.aktanazat.sona.mobile.watchkitapp` |
| Embedded keyboard        | `com.aktanazat.sona.mobile.keyboard`    |

The Mac project uses automatic development signing for team `AAVB324H37`. The app and camera extension share app group `AAVB324H37.com.aktanazat.sona.camera`; the app also needs the System Extension install capability. A local signed build requires that team's development certificate/private key and matching profiles. The bundle phase signs both private executables and `SonaCore.app` with Xcode's selected identity; Xcode signs the camera extension and outer app. No ad-hoc identity is substituted when signing is enabled.

After preparing the release engine and helpers, a signed development archive can be made on a Mac with those credentials:

```bash
xcodebuild -project macos/Sona.xcodeproj -scheme Sona \
  -configuration Release -destination 'generic/platform=macOS' \
  -archivePath /private/var/tmp/sona-native-signed/Sona.xcarchive \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
  SONA_CORE_TARGET_DIR="$PWD/src-tauri/target" \
  -allowProvisioningUpdates archive
bash macos/scripts/inspect-package.sh app \
  /private/var/tmp/sona-native-signed/Sona.xcarchive/Products/Applications/Sona.app arm64
codesign --verify --deep --strict \
  /private/var/tmp/sona-native-signed/Sona.xcarchive/Products/Applications/Sona.app
```

A development signature is not a Developer ID distribution signature or notarization. Public Mac distribution additionally requires a Developer ID Application certificate/private key, compatible app and camera-extension provisioning profiles, and notarization access. Export the archive through Xcode Organizer with the intended distribution credentials, notarize the exported app, staple its ticket, and check both `codesign --verify --deep --strict` and `spctl --assess --type execute` on that exported bundle before publishing it. Those credentials are not configured in this repository's Actions secrets, so the tag workflow does not claim to perform that export.

When upgrading a signed installation, replace `/Applications/Sona.app` as a complete bundle with the same registered identities and team. Do not overwrite its executable in place, reset permission grants, change its data directory, or register a second runnable copy as part of an upgrade. A signing-team change needs its own migration decision.

### CI and tag drafts

`.github/workflows/native.yml` builds the native Mac Release bundle and the iPhone simulator app on pushes to `main`, pull requests, and manual runs. The Mac job uses the reusable `build.yml`: it builds both helpers and the release engine, builds the native Xcode app, inspects the complete payload, and stores `Sona-macos-arm64-unsigned.zip`. The phone job checks that the Watch app and keyboard are embedded. Simulator output is not an installable `.ipa`.

Pushing a `vX.Y.Z` tag runs `release.yml`. It uses that same Mac build, requires the tag, core version, and native app version to agree, then attaches the explicitly unsigned ZIP to a draft GitHub release. It refuses to modify an already-published release. A successful draft job proves the unsigned package was built and inspected, not that it passed Gatekeeper or can be distributed as signed software. Review and signed distribution remain separate.

There are no Tauri web-app, browser UI, Windows, Linux, or Nix package release jobs.

## Build the iPhone app

The `Sona` scheme builds the phone app, its embedded Watch companion, and its keyboard. With Xcode 26 and the simulator SDKs installed:

```bash
xcodebuild -project mobile/Sona.xcodeproj -scheme Sona \
  -configuration Debug -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /private/var/tmp/sona-iphone \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build
```

Physical-device builds and App Store exports need an Apple account authorized for team `AAVB324H37`, signing certificates/private keys, and profiles for the phone, Watch app, and keyboard with their existing capabilities. They are manual; no phone distribution secrets or automated upload are configured here.

The existing archive command creates a Release archive and exports a signed `.ipa` with `mobile/ExportOptions.plist`. It allows Xcode to update provisioning and does not upload the result:

```bash
bash mobile/scripts/archive.sh /private/var/tmp/sona-iphone-release
```

Pairing and encrypted sync still need the companion service in `cloudflare/sona-companion/`. A successful simulator build does not prove real microphone, keyboard full-access, Watch transfer, pairing, or device signing behavior.

## Package layout and CLI

```text
Sona.app/Contents/
  MacOS/Sona
  Helpers/SonaCore.app/Contents/
    Info.plist
    MacOS/sona-core
    MacOS/sona-agent-hook
    MacOS/sona-cleanup
    Resources/resources/
    Resources/_up_/LICENSE
    Resources/_up_/NOTICE
  Library/SystemExtensions/com.aktanazat.sona.mac.camera.systemextension/
```

Open `Sona.app` for the desktop interface. The native app launches `sona-core --native-socket` and owns the child process. The engine's Tauri configuration remains for Rust resource/context generation, not for a web frontend or a separate desktop bundle.

The installed command-line engine is:

```bash
/Applications/Sona.app/Contents/Helpers/SonaCore.app/Contents/MacOS/sona-core --help
```

Its headless transcription and corpus commands remain available. External corpus reads require Settings > Agents > External access. `--loop-resolve` also requires External mutations. Both grants start off. The MCP server in `tools/sona-mcp/` wraps that same CLI; see `skills/sona/SKILL.md` for the command contract.

Portable mode is for isolated core runs, not a separate shipped desktop app. A `portable` marker beside the core executable with `Sona Portable Mode` keeps its data in adjacent `Data/`; debug cores also require `SONA_ALLOW_PORTABLE_DEV=1`. Native credential storage is disabled there. Never use the installed app or the default user profile as a disposable verification environment.

## Checks

Run the applicable checks once after integration. Keep tests for observable behavior, data integrity, privacy, public contracts, and measured resource limits. A meaningful failing test stays.

```bash
bun run format:check
bun run check:identity
bun run lint:anti-slop
bun run test:scripts
bun run typecheck:mcp
bun run test:mcp
bun run check:onboarding
bun scripts/cargo-target-dir.ts cargo check --manifest-path src-tauri/Cargo.toml --all-features
bun run test:backend
bun scripts/cargo-target-dir.ts cargo test --manifest-path src-tauri/Cargo.toml --bin sona-agent-hook
ast-grep scan -c tools/ast-grep/anti-slop/sgconfig.yml
```

For the companion service, run its checks from `cloudflare/sona-companion/`:

```bash
bun install --frozen-lockfile
bun run typecheck
bun run test
```

The native builds and package inspection above are separate from the test suites. Verify recording, socket requests, consent, permissions, pairing, sync, and the native screens in an isolated profile/device before shipping. Inspect matching before/after screens in light and dark appearances; build output is not visual proof.

### Real meeting acceptance

The active local meeting acceptance is `meeting::processing::real_engine_acceptance::tests::actual_apple_long_meeting_pipeline_and_regeneration`. It requires a ready Apple Intelligence model and uses the real imported-meeting pipeline, not a model substitute. It checks the original long conversation, then checks regenerated notes against the same decisions, owners, revised deadlines, and citations. Requests and results are saved under `.progress/meeting-acceptance/`; the original request and reply-token limits remain enforced at the SDK boundary. A cloud-only pass does not replace this check.

The native ledger presents each literal excerpt with its original source reference and speaker. A speaker is not automatically a task owner, and the person who settles a choice never owns it: a speaker's own undertaking is a personal promise. A promise version whose deadline is not in its own cited excerpts, or whose excerpts do not exist, is dropped. When a settled choice and a promise select exactly the same excerpts, the app keeps one role: the promise when the choice names the same person, the choice otherwise. Stances use the same bounded excerpt receipts as other ledger facts, and the app restores all original source references before canonical validation. The separate real-model named-assignment check is `actual_apple_named_assignment_is_not_the_speaker`; run it explicitly with `--ignored`. Passing that check does not replace the complete initial and regenerated acceptance.

A long meeting's ledger is read in parts and the parts are joined by the app, not re-read by the model. Threads, commitments, stances, and open questions keep the roles their part gave them; identical subjects with the same state and owner become one thread with every source reference. One small model request writes the headline and may name promises that are versions of the same work by the same person, a later closed thread that cancels a promise, or a later thread that answers an open question. The app accepts a cancellation or an answer only from a thread whose source comes after the promise or question. These checks do not establish real-model accuracy.

```bash
bun scripts/cargo-target-dir.ts cargo test --manifest-path src-tauri/Cargo.toml --lib --features cloud-realtime actual_apple_long_meeting_pipeline_and_regeneration -- --nocapture --test-threads=1
```

## Version bump

1. Update `[package] version` in `src-tauri/Cargo.toml` and the `sona` entry in `src-tauri/Cargo.lock`.
2. Match root `package.json` to that core version.
3. Match `MARKETING_VERSION` in the native Mac project and increase `CURRENT_PROJECT_VERSION` for the app and camera extension. The core helper takes those values from Xcode.
4. Update the iPhone, Watch, and keyboard versions/build numbers together in the mobile project when shipping the phone app.

`src-tauri/tauri.conf.json` has no separate version field. Rust derives its package version from Cargo. The native release inspection rejects a version mismatch rather than relabeling an old core. The MCP and companion-service packages version independently.

## Migrating from the Legacy app

On first launch Sona offers to move settings, history, recordings, models, and configured provider keys from the Legacy app. Close the Legacy app first. Sona copies mutable data, moves large blobs, journals each operation, and writes a completion receipt only after migration finishes.

If migration needs to be reversed, use the debug rollback action while the Legacy app is closed. The rollback discards Sona-era settings and history changes, then moves recordings and models back. It does not uninstall the Legacy app or re-register its macOS login item.
