---
name: verify
description: Smoke-test a change against a real running Sona core before committing or calling a change done in this repo. Builds the debug core, runs it as a throwaway portable copy, drives it over the native shell socket and checks the headless corpus verbs.
---

# Verify

Run from the checkout root:

```bash
bun .claude/skills/verify/smoke.ts
```

Pass: exit 0 and the last line is `[verify] PASS`. Anything else prints
`[verify] FAIL: <reason>` and exits 1. After the build the run takes about
4 seconds. `--no-build` skips the build and runs the binary already in the
target directory; step 3 still refuses one built from other Rust sources.

What it does, in order:

1. `cargo build --bin sona` through `scripts/cargo-target-dir.ts`.
2. Recreates a portable profile at `/private/var/tmp/sona-verify-<checkout>`
   (override with `SONA_VERIFY_PROFILE`): the binary, the `portable` marker,
   and a settings store with meeting detection off, so the run never raises
   a meeting prompt for a call happening on this Mac.
3. `sona --version` must stamp a commit whose `src-tauri/` matches `HEAD`,
   and must say `-dirty` when `src-tauri/` has uncommitted edits. A stale
   binary fails here.
4. `sona --meetings --last 1` must refuse with exit 1 and
   `consent_required`: External access is off on a new profile.
5. Starts the core with `--native-socket`, the way the macOS app does, and
   waits for the socket, which is bound only after every manager is up.
   Over it: `is_portable` is true, `change_external_query_enabled_setting`
   turns External access on, then `shutdown`; the core must exit 0.
6. `sona --meetings --last 1` must now pass the consent gate and stop at
   the corpus with exit 1 and `unavailable`. The corpus key lives in the
   keychain, which portable mode never touches. This proves the core wrote
   the setting the headless plane reads.
7. No process is left running from the profile and the core log has no
   panic.

The headless checks drop the two `[portable] ...` lines a portable copy
prints on stderr; anything else on stderr fails the run.

When a change alters one of these contracts, change the check with it. When
a change adds or changes a command the shell sends, add a call over the
socket in `driveCore`.

## Not covered

- Frontend (`src/`): also run `bun run build` (typecheck and static export).
- The SwiftUI shell (`macos/`): needs Xcode and the team's signing.
- Anything that reads the encrypted corpus or other keychain-backed state.
- Rust unit tests: `bun run test:backend`.

## Rules

- Never point this at the installed app or a run on the default data
  directory. The portable profile keeps the check off the owner's settings.
- A cold debug build of this crate is large. Check free disk before
  building in a fresh checkout.
- If the build fails on a missing `src-tauri/binaries/sona-agent-hook-*`,
  run `bun run prepare:agent-hook -- aarch64-apple-darwin` once. It does a
  release build of the whole crate.
- If `cargo` is not on `PATH` (Homebrew rustup), add
  `/opt/homebrew/opt/rustup/bin`.

## Cleanup

The script stops the core itself and kills it if `shutdown` fails. The
profile stays for inspection (`core.log`, `Data/logs/`). Remove it with
`rm -rf /private/var/tmp/sona-verify-<checkout>` when done.
