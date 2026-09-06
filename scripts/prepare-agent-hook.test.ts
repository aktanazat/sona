import { afterEach, describe, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { delimiter, join } from "node:path";
import { tmpdir } from "node:os";
import {
  configWithoutExternalBinaries,
  sidecarFilename,
  stageSidecar,
} from "./prepare-agent-hook";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { force: true, recursive: true });
  }
});

function temporaryDirectory(): string {
  const directory = mkdtempSync(join(tmpdir(), "sona-agent-hook-"));
  temporaryDirectories.push(directory);
  return directory;
}

function writeExecutable(path: string, contents: string): void {
  writeFileSync(path, contents);
  chmodSync(path, 0o755);
}

describe("prepare-agent-hook", () => {
  test("uses Tauri's target-suffixed sidecar name", () => {
    expect(sidecarFilename("aarch64-apple-darwin")).toBe(
      "sona-agent-hook-aarch64-apple-darwin",
    );
  });

  test("adds the Windows executable suffix after the target triple", () => {
    expect(sidecarFilename("x86_64-pc-windows-msvc")).toBe(
      "sona-agent-hook-x86_64-pc-windows-msvc.exe",
    );
  });

  test("removes external binaries only for the helper Cargo build", () => {
    const config = JSON.parse(
      configWithoutExternalBinaries(
        JSON.stringify({
          bundle: {
            externalBin: ["binaries/sona-agent-hook"],
            resources: ["resources/**/*"],
          },
          build: { beforeBuildCommand: "bun run build" },
        }),
      ),
    );

    expect(config).toEqual({
      bundle: { externalBin: [], resources: ["resources/**/*"] },
      build: { beforeBuildCommand: "bun run build" },
    });
  });

  test("fails when Cargo did not produce the hook binary", () => {
    const directory = temporaryDirectory();
    const destination = join(directory, "binaries", "sona-agent-hook-test");

    expect(() =>
      stageSidecar(
        join(directory, "missing-hook"),
        destination,
        "aarch64-apple-darwin",
      ),
    ).toThrow("build output is missing");
    expect(existsSync(destination)).toBeFalse();
  });

  test("replaces a stale staged sidecar", () => {
    const directory = temporaryDirectory();
    const source = join(directory, "sona-agent-hook");
    const destination = join(directory, "binaries", "sona-agent-hook-test");
    writeExecutable(source, "current hook binary");
    mkdirSync(join(directory, "binaries"), { recursive: true });
    writeFileSync(destination, "stale hook binary");

    stageSidecar(source, destination, "aarch64-apple-darwin");

    expect(readFileSync(destination, "utf8")).toBe("current hook binary");
    expect(statSync(destination).mode & 0o111).not.toBe(0);
  });

  function stageCargoTargetHelper(
    profile: "release" | "debug",
    tauriDebug: "false" | "true",
  ) {
    const directory = temporaryDirectory();
    const root = join(directory, "project");
    const target = "aarch64-apple-darwin";
    const cacheRoot = join(root, "isolated-target");
    const bin = join(directory, "bin");
    const payload = `${profile} target helper`;
    mkdirSync(join(root, "src-tauri"), { recursive: true });
    mkdirSync(bin, { recursive: true });
    writeExecutable(
      join(bin, "cargo"),
      `#!/bin/sh
set -eu
profile=debug
target=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--release" ]; then
    profile=release
    shift
  elif [ "$1" = "--target" ]; then
    target="$2"
    shift 2
  else
    shift
  fi
done
mkdir -p "$CARGO_TARGET_DIR/$target/$profile"
printf '%s' "$SONA_HOOK_PAYLOAD" > "$CARGO_TARGET_DIR/$target/$profile/sona-agent-hook"
chmod 755 "$CARGO_TARGET_DIR/$target/$profile/sona-agent-hook"
`,
    );
    const runner = join(directory, "run-prepare-agent-hook.ts");
    writeFileSync(
      runner,
      `import { prepareAgentHook } from ${JSON.stringify(
        new URL("./prepare-agent-hook.ts", import.meta.url).href,
      )};
prepareAgentHook("${target}", process.argv[2]);
`,
    );

    const result = Bun.spawnSync([process.execPath, runner, root], {
      env: {
        ...process.env,
        CARGO_TARGET_DIR: "isolated-target",
        PATH: [bin, process.env.PATH].filter(Boolean).join(delimiter),
        SONA_HOOK_PAYLOAD: payload,
        TAURI_ENV_DEBUG: tauriDebug,
      },
      stderr: "pipe",
      stdout: "pipe",
    });

    /* Whatever directory the script handed cargo is the one the Tauri bundler
     * will read the stripped copy from, so the assertions below start from
     * cargo's own output rather than a path this fixture guessed. */
    const children = existsSync(cacheRoot) ? readdirSync(cacheRoot) : [];
    expect(children).toHaveLength(1);
    const targetDirectory = join(cacheRoot, children[0] ?? "");

    return { payload, result, root, target, targetDirectory };
  }

  test("stages a Cargo target helper for a release Tauri build", () => {
    const fixture = stageCargoTargetHelper("release", "false");

    expect(fixture.result.exitCode).toBe(0);
    expect(
      readFileSync(
        join(
          fixture.root,
          "src-tauri",
          "binaries",
          sidecarFilename(fixture.target),
        ),
        "utf8",
      ),
    ).toBe(fixture.payload);
    expect(
      readFileSync(
        join(fixture.targetDirectory, "release", "agent-hook"),
        "utf8",
      ),
    ).toBe(fixture.payload);
  });

  test("stages a Cargo target helper for a debug Tauri build", () => {
    const fixture = stageCargoTargetHelper("debug", "true");

    expect(fixture.result.exitCode).toBe(0);
    expect(
      readFileSync(
        join(
          fixture.root,
          "src-tauri",
          "binaries",
          sidecarFilename(fixture.target),
        ),
        "utf8",
      ),
    ).toBe(fixture.payload);
    expect(
      readFileSync(
        join(fixture.targetDirectory, "debug", "agent-hook"),
        "utf8",
      ),
    ).toBe(fixture.payload);
  });
});
