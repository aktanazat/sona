/* Run a build or test command with a Cargo target directory this checkout owns.
 *
 * Cargo hashes a local package's unit against a workspace-relative path, so a
 * checkout that moves keeps its cache. Two checkouts of this repo pointed at
 * one `CARGO_TARGET_DIR` therefore resolve to the same unit, and cargo's
 * staleness test for it compares mtimes: the checkout whose files are older
 * silently inherits the other one's compiled artifacts. On 2026-09-04 a
 * throwaway worktree built into this tree's target directory and left the main
 * tree linking the worktree's `libmeeting_capture.a`, which is missing the
 * export the tests here need; a probe the next day watched the plain Rust half
 * of it, `cargo run` in one tree executing the other tree's binary with
 * nothing recompiled.
 *
 * Cargo's own default is already per-checkout, so with `CARGO_TARGET_DIR`
 * unset this passes the command through untouched. A set value is treated as a
 * cache root — the reason to set one is a faster disk or an SSD you keep out of
 * backups — and each checkout gets its own directory beneath it. */
import { createHash } from "node:crypto";
import { basename, join, resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");

type Environment = Record<string, string | undefined>;

/** The target directory to hand cargo, or `undefined` to leave cargo's default. */
export function resolveCargoTargetDirectory(
  root: string = ROOT,
  environment: Environment = process.env,
): string | undefined {
  const configured = environment.CARGO_TARGET_DIR?.trim();
  if (!configured) {
    return undefined;
  }
  /* Against the checkout, not the caller: `tauri build` runs its
   * `beforeBuildCommand` from the repo root and cargo from `src-tauri`, and a
   * relative cache root has to mean one directory across both. */
  const cacheRoot = resolve(root, configured);
  /* Readable first and unique second: two worktrees are often both called
   * `wt`, and only the absolute path tells them apart. */
  const digest = createHash("sha256").update(root).digest("hex").slice(0, 12);
  const directoryName = `${basename(root)}-${digest}`;
  /* Idempotent, because these commands nest: `tauri build` runs
   * `beforeBuildCommand`, which comes back through this script with the
   * derived value already in the environment. */
  return basename(cacheRoot) === directoryName
    ? cacheRoot
    : join(cacheRoot, directoryName);
}

function main(): void {
  const [command, ...commandArguments] = process.argv.slice(2);
  if (!command) {
    throw new Error("a command to run is required");
  }
  const targetDirectory = resolveCargoTargetDirectory();
  const result = Bun.spawnSync([command, ...commandArguments], {
    env: targetDirectory
      ? { ...process.env, CARGO_TARGET_DIR: targetDirectory }
      : process.env,
    stdio: ["inherit", "inherit", "inherit"],
  });
  process.exitCode = result.exitCode ?? 1;
}

if (import.meta.main) {
  try {
    main();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`[cargo-target-dir] ${message}`);
    process.exitCode = 1;
  }
}
