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
import { z } from "zod";
import { resolveCargoTargetDirectory } from "./cargo-target-dir";

const BINARY_NAME = "sona-agent-hook";
const ROOT = resolve(import.meta.dirname, "..");

type Environment = Record<string, string | undefined>;

export function sidecarFilename(targetTriple: string): string {
  const suffix = targetTriple.includes("-windows-") ? ".exe" : "";
  return `${BINARY_NAME}-${targetTriple}${suffix}`;
}

function validateTargetTriple(targetTriple: string): string {
  const normalized = targetTriple.trim();
  if (!normalized || normalized !== targetTriple) {
    throw new Error("a non-empty Cargo target triple is required");
  }
  if (normalized.includes("/") || normalized.includes("\\")) {
    throw new Error(
      "the Cargo target triple must not contain a path separator",
    );
  }
  return normalized;
}

export function resolveTargetTriple(
  arguments_: readonly string[],
  environment: Environment = process.env,
): string {
  const target = arguments_[0] ?? environment.TAURI_ENV_TARGET_TRIPLE ?? "";
  return validateTargetTriple(target);
}

/* Tauri's `--config` override is arbitrary JSON. This script rewrites exactly
 * one field of it and passes everything else through verbatim, so the schema
 * below decodes it *as* JSON rather than claiming to know a shape it never
 * reads. `bundle` is the one field it does read, and it has to be an object
 * for the merge below to mean anything. */
type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue };

const jsonValueSchema: z.ZodType<JsonValue> = z.lazy(() =>
  z.union([
    z.string(),
    z.number(),
    z.boolean(),
    z.null(),
    z.array(jsonValueSchema),
    z.record(jsonValueSchema),
  ]),
);

const configOverrideSchema = z.record(jsonValueSchema);
const bundleOverrideSchema = z.record(jsonValueSchema).optional();

export function configWithoutExternalBinaries(configOverride?: string): string {
  const config = configOverrideSchema.parse(JSON.parse(configOverride ?? "{}"));
  const bundle = bundleOverrideSchema.parse(config.bundle);
  return JSON.stringify({
    ...config,
    bundle: {
      ...bundle,
      externalBin: [],
    },
  });
}

export function assertBuiltExecutable(
  path: string,
  targetTriple: string,
): void {
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
  if (!targetTriple.includes("-windows-")) {
    try {
      accessSync(path, constants.X_OK);
    } catch {
      throw new Error(
        `sona-agent-hook build output is not executable: ${path}`,
      );
    }
  }
}

export function stageSidecar(
  source: string,
  destination: string,
  targetTriple: string,
): void {
  assertBuiltExecutable(source, targetTriple);
  mkdirSync(dirname(destination), { recursive: true });
  rmSync(destination, { force: true });
  copyFileSync(source, destination);
  chmodSync(destination, statSync(source).mode & 0o777);
  assertBuiltExecutable(destination, targetTriple);
}

export function prepareAgentHook(targetTriple: string, root = ROOT): string {
  const target = validateTargetTriple(targetTriple);
  const cargoWorkingDirectory = join(root, "src-tauri");
  const buildProfile =
    process.env.TAURI_ENV_DEBUG === "true" ? "debug" : "release";
  /* The build below and the lookup after it have to name one directory, so
   * both take it from here - and it is the same value `bun run tauri` hands
   * cargo, so a `tauri build` that comes back through this script stages the
   * binary that build produced. */
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
      TAURI_CONFIG: configWithoutExternalBinaries(process.env.TAURI_CONFIG),
    },
    stdio: ["inherit", "inherit", "inherit"],
  });
  if (result.exitCode !== 0) {
    throw new Error(`cargo failed to build sona-agent-hook for ${target}`);
  }

  const source = join(
    targetDirectory,
    target,
    buildProfile,
    `${BINARY_NAME}${target.includes("-windows-") ? ".exe" : ""}`,
  );
  const destination = join(
    root,
    "src-tauri",
    "binaries",
    sidecarFilename(target),
  );
  stageSidecar(source, destination, target);

  /* The Tauri bundler strips the app's main-binary prefix ("sona-") when it
   * derives the bundled sidecar name, then copies it from the matching profile.
   * Without this copy, bundling fails to find ".../target/<profile>/agent-hook". */
  const strippedName = `agent-hook${target.includes("-windows-") ? ".exe" : ""}`;
  const hostProfile = join(targetDirectory, buildProfile);
  stageSidecar(source, join(hostProfile, strippedName), target);
  return destination;
}

function main(): void {
  const target = resolveTargetTriple(process.argv.slice(2));
  const destination = prepareAgentHook(target);
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
