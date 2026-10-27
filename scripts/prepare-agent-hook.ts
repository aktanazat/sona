import {
  accessSync,
  chmodSync,
  constants,
  copyFileSync,
  lstatSync,
  mkdirSync,
  rmSync,
  statSync,
  type Stats,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { resolveCargoTargetDirectory } from "./cargo-target-dir";

const BINARY_NAME = "sona-agent-hook";
const ROOT = resolve(import.meta.dirname, "..");

export function sidecarFilename(targetTriple: string): string {
  return `${BINARY_NAME}-${validateTargetTriple(targetTriple)}`;
}

function validateTargetTriple(targetTriple: string): string {
  if (
    targetTriple !== "aarch64-apple-darwin" &&
    targetTriple !== "x86_64-apple-darwin"
  ) {
    throw new Error("a native macOS Cargo target triple is required");
  }
  return targetTriple;
}

export function resolveTargetTriple(arguments_: readonly string[]): string {
  const targetIndex = arguments_[0] === "--" ? 1 : 0;
  return validateTargetTriple(arguments_[targetIndex] ?? "");
}

export function assertBuiltExecutable(path: string): void {
  let metadata: Stats;
  try {
    metadata = lstatSync(path);
  } catch {
    throw new Error(`sona-agent-hook build output is missing: ${path}`);
  }

  if (!metadata.isFile()) {
    throw new Error(
      `sona-agent-hook build output is not a regular file: ${path}`,
    );
  }
  if (metadata.size === 0) {
    throw new Error(`sona-agent-hook build output is empty: ${path}`);
  }
  try {
    accessSync(path, constants.X_OK);
  } catch {
    throw new Error(`sona-agent-hook build output is not executable: ${path}`);
  }
}

export function stageSidecar(source: string, destination: string): void {
  assertBuiltExecutable(source);
  mkdirSync(dirname(destination), { recursive: true });
  rmSync(destination, { force: true });
  copyFileSync(source, destination);
  chmodSync(destination, statSync(source).mode & 0o777);
  assertBuiltExecutable(destination);
}

export function prepareAgentHook(
  targetTriple: string,
  root = ROOT,
  buildProfile: "release" | "debug" = "release",
): string {
  const target = validateTargetTriple(targetTriple);
  const cargoWorkingDirectory = join(root, "src-tauri");
  // Cargo and the artifact lookup must resolve the same checkout-owned root.
  const targetDirectory =
    resolveCargoTargetDirectory(root) ?? join(cargoWorkingDirectory, "target");
  const cargoArguments = ["cargo", "build"];
  if (buildProfile === "release") cargoArguments.push("--release");
  cargoArguments.push("--bin", BINARY_NAME, "--target", target);
  const result = Bun.spawnSync(cargoArguments, {
    cwd: cargoWorkingDirectory,
    env: {
      ...process.env,
      CARGO_TARGET_DIR: targetDirectory,
    },
    stdio: ["inherit", "inherit", "inherit"],
  });
  if (result.exitCode !== 0) {
    throw new Error(`cargo failed to build sona-agent-hook for ${target}`);
  }

  const source = join(targetDirectory, target, buildProfile, BINARY_NAME);
  const destination = join(
    root,
    "src-tauri",
    "binaries",
    sidecarFilename(target),
  );
  stageSidecar(source, destination);

  // A SONA_CORE development override finds the real hook beside the core,
  // just as the native app's helper bundle does.
  const hostProfile = join(targetDirectory, buildProfile);
  stageSidecar(source, join(hostProfile, BINARY_NAME));
  return destination;
}

function main(): void {
  const target = resolveTargetTriple(process.argv.slice(2));
  const profile = process.argv.slice(3).includes("--debug")
    ? "debug"
    : "release";
  const destination = prepareAgentHook(target, ROOT, profile);
  console.log(`[prepare-agent-hook] staged ${relative(ROOT, destination)}`);
}

if (import.meta.main) {
  try {
    main();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`[prepare-agent-hook] ${message}`);
    process.exitCode = 1;
  }
}
