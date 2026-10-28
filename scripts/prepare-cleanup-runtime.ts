import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { resolveCargoTargetDirectory } from "./cargo-target-dir";
import { resolveTargetTriple } from "./prepare-agent-hook";
import tauriConfig from "../src-tauri/tauri.conf.json";

const ROOT = resolve(import.meta.dirname, "..");

async function run(arguments_: string[]): Promise<void> {
  const child = Bun.spawn(arguments_, {
    cwd: ROOT,
    stdio: ["inherit", "inherit", "inherit"],
  });
  const status = await child.exited;
  if (status !== 0) {
    throw new Error(`${arguments_[0]} exited with status ${status}`);
  }
}

async function main(): Promise<void> {
  const target = resolveTargetTriple(process.argv.slice(2));
  const targetRoot =
    resolveCargoTargetDirectory(ROOT) ?? join(ROOT, "src-tauri", "target");
  const build = join(targetRoot, "cleanup-runtime", target);
  const destination = join(ROOT, "src-tauri", "resources", "cleanup-runtime");
  const staging = join(build, "stage");
  const binaries = join(ROOT, "src-tauri", "binaries");
  const options = ["-DCMAKE_BUILD_TYPE=Release"];
  const architecture = target.startsWith("aarch64-") ? "arm64" : "x86_64";
  const minimumVersion = tauriConfig.bundle.macOS.minimumSystemVersion;
  const deploymentTarget =
    Number(minimumVersion.split(".")[0]) < 11 ? "11.0" : minimumVersion;
  options.push(
    `-DCMAKE_OSX_ARCHITECTURES=${architecture}`,
    `-DCMAKE_OSX_DEPLOYMENT_TARGET=${deploymentTarget}`,
  );
  for (const key of [
    "CMAKE_C_COMPILER",
    "CMAKE_CXX_COMPILER",
    "CMAKE_C_COMPILER_TARGET",
    "CMAKE_CXX_COMPILER_TARGET",
    "CMAKE_LINKER",
    "CMAKE_MT",
    "CMAKE_RC_COMPILER",
    "CMAKE_C_FLAGS",
    "CMAKE_CXX_FLAGS",
    "CMAKE_POLICY_VERSION_MINIMUM",
  ]) {
    const value = process.env[key];
    if (value) options.push(`-D${key}=${value}`);
  }
  await run([
    "cmake",
    "-S",
    join(ROOT, "src-tauri", "cleanup-runtime"),
    "-B",
    build,
    ...options,
  ]);
  await run([
    "cmake",
    "--build",
    build,
    "--target",
    "sona-cleanup",
    "--config",
    "Release",
    "--parallel",
    "4",
  ]);
  await run([
    "cmake",
    "--install",
    build,
    "--prefix",
    staging,
    "--config",
    "Release",
    "--component",
    "Runtime",
  ]);
  mkdirSync(destination, { recursive: true });
  mkdirSync(binaries, { recursive: true });
  const executable = join(staging, "sona-cleanup");
  const sidecar = join(binaries, `sona-cleanup-${target}`);
  const development = join(binaries, "sona-cleanup");
  copyFileSync(executable, sidecar);
  copyFileSync(executable, development);
  for (const license of ["llama-LICENSE", "json-LICENSE", "model-LICENSE"]) {
    copyFileSync(join(staging, license), join(destination, license));
  }
  chmodSync(sidecar, 0o755);
  chmodSync(development, 0o755);
  console.log(`[cleanup-runtime] staged ${sidecar}`);
}

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    console.error(
      `[cleanup-runtime] ${error instanceof Error ? error.message : String(error)}`,
    );
    process.exitCode = 1;
  }
}
