import { afterEach, describe, expect, test } from "bun:test";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { force: true, recursive: true });
  }
});

/* macOS hands out `/var/...` temp paths for a `/private/var` directory, and
 * the script answers with the resolved form. */
function temporaryDirectory(prefix: string): string {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), prefix)));
  temporaryDirectories.push(directory);
  return directory;
}

const WRAPPER = resolve(import.meta.dirname, "cargo-target-dir.ts");

/* The contract is what the wrapped command sees, so every case below runs the
 * script and reads the environment on the other side of it. */
function runWrapped(environment: Record<string, string>, ...command: string[]) {
  const result = Bun.spawnSync(["bun", WRAPPER, ...command], {
    env: { PATH: process.env.PATH ?? "", ...environment },
  });
  return {
    stdout: result.stdout.toString().trim(),
    exitCode: result.exitCode ?? 1,
  };
}

/** A checkout that owns a copy of the script, the way a worktree does. */
function checkoutWithScript(name: string): string {
  const root = join(temporaryDirectory("cargo-target-dir-"), name);
  mkdirSync(join(root, "scripts"), { recursive: true });
  copyFileSync(WRAPPER, join(root, "scripts", "cargo-target-dir.ts"));
  return root;
}

describe("cargo-target-dir", () => {
  test("leaves cargo's own per-checkout default alone when nothing is set", () => {
    const run = runWrapped({}, "printenv", "CARGO_TARGET_DIR");
    expect(run.stdout).toBe("");
    expect(run.exitCode).toBe(1);
  });

  /* `export CARGO_TARGET_DIR=` in a shell profile is a set variable with
   * nothing in it, and inventing a directory out of it would put the build
   * somewhere nobody named. */
  test("treats an empty setting as no setting", () => {
    const run = runWrapped(
      { CARGO_TARGET_DIR: "  " },
      "printenv",
      "CARGO_TARGET_DIR",
    );
    expect(run.stdout).toBe("");
  });

  test("builds under a set cache root instead of it", () => {
    const cacheRoot = temporaryDirectory("cargo-cache-");
    const run = runWrapped(
      { CARGO_TARGET_DIR: cacheRoot },
      "printenv",
      "CARGO_TARGET_DIR",
    );
    expect(dirname(run.stdout)).toBe(cacheRoot);
  });

  test("gives two checkouts of the same name different directories", () => {
    const cacheRoot = temporaryDirectory("cargo-cache-");
    const [first, second] = ["sona", "sona"].map((name) =>
      Bun.spawnSync(
        [
          "bun",
          join(checkoutWithScript(name), "scripts", "cargo-target-dir.ts"),
          "printenv",
          "CARGO_TARGET_DIR",
        ],
        { env: { PATH: process.env.PATH ?? "", CARGO_TARGET_DIR: cacheRoot } },
      )
        .stdout.toString()
        .trim(),
    );
    expect(dirname(first)).toBe(cacheRoot);
    expect(dirname(second)).toBe(cacheRoot);
    expect(first).not.toBe(second);
  });

  test("hands a nested invocation the directory it already derived", () => {
    const cacheRoot = temporaryDirectory("cargo-cache-");
    const once = runWrapped(
      { CARGO_TARGET_DIR: cacheRoot },
      "printenv",
      "CARGO_TARGET_DIR",
    );
    const twice = runWrapped(
      { CARGO_TARGET_DIR: cacheRoot },
      "bun",
      WRAPPER,
      "printenv",
      "CARGO_TARGET_DIR",
    );
    expect(twice.stdout).toBe(once.stdout);
  });

  test("resolves a relative cache root against the checkout, not the caller", () => {
    const root = checkoutWithScript("sona");
    const run = Bun.spawnSync(
      [
        "bun",
        join(root, "scripts", "cargo-target-dir.ts"),
        "printenv",
        "CARGO_TARGET_DIR",
      ],
      {
        cwd: join(root, "scripts"),
        env: { PATH: process.env.PATH ?? "", CARGO_TARGET_DIR: "cache" },
      },
    );
    expect(dirname(run.stdout.toString().trim())).toBe(join(root, "cache"));
  });

  test("passes the command's arguments and exit code through", () => {
    const run = runWrapped({}, "printf", "%s-%s", "one", "two");
    expect(run.stdout).toBe("one-two");
    expect(runWrapped({}, "false").exitCode).toBe(1);
  });
});
