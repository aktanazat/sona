/* Smoke test for the Rust core: build it, run it as a portable copy, drive it
 * over the native shell socket, and check the headless corpus plane against
 * the settings it wrote. Nothing here reads or writes the installed app's
 * data: portable mode keeps settings, logs and the corpus under the profile's
 * own Data/ directory. Exit 0 means every check passed. */
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import {
  constants,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createConnection, type Socket } from "node:net";
import { basename, join, resolve } from "node:path";
import { resolveCargoTargetDirectory } from "../../../scripts/cargo-target-dir";

const ROOT = resolve(import.meta.dirname, "../../..");
const MARKER = "Sona Portable Mode";
const PROFILE =
  process.env.SONA_VERIFY_PROFILE ??
  `/private/var/tmp/sona-verify-${basename(ROOT)}`;
const BINARY = join(PROFILE, "sona");
const SOCKET = join(PROFILE, "core.sock");
const CORE_LOG = join(PROFILE, "core.log");
const ENV = { ...process.env, SONA_ALLOW_PORTABLE_DEV: "1" };

function step(message: string): void {
  console.log(`[verify] ${message}`);
}

function fail(message: string): never {
  throw new Error(message);
}

function build(): void {
  step("building src-tauri bin `sona` (debug)");
  const result = spawnSync(
    "bun",
    ["../scripts/cargo-target-dir.ts", "cargo", "build", "--bin", "sona"],
    { cwd: join(ROOT, "src-tauri"), stdio: "inherit" },
  );
  if (result.status !== 0) fail(`cargo build exited with ${result.status}`);
}

/* Only a directory this script made may be wiped: one holding the portable
 * marker, or an empty one. */
function freshProfile(): void {
  if (existsSync(PROFILE)) {
    const marker = join(PROFILE, "portable");
    const ours =
      readdirSync(PROFILE).length === 0 ||
      (existsSync(marker) && readFileSync(marker, "utf8").trim() === MARKER);
    if (!ours)
      fail(`${PROFILE} exists and is not a portable profile; not touching it`);
    rmSync(PROFILE, { recursive: true, force: true });
  }
  mkdirSync(join(PROFILE, "Data"), { recursive: true });
  const target =
    resolveCargoTargetDirectory(ROOT) ?? join(ROOT, "src-tauri", "target");
  copyFileSync(
    join(target, "debug", "sona"),
    BINARY,
    constants.COPYFILE_FICLONE,
  );
  writeFileSync(join(PROFILE, "portable"), MARKER);
  /* Meeting detection is on by default and watches meeting apps and browser
   * tabs on this Mac; a smoke run must not raise a meeting prompt. The
   * core salvages the rest of the settings document from defaults. */
  writeFileSync(
    join(PROFILE, "Data", "settings_store.json"),
    JSON.stringify({ settings: { detection_enabled: false } }),
  );
  step(`profile ${PROFILE}`);
}

type Run = { code: number | null; stdout: string; stderr: string };

function headless(args: string[]): Run {
  const result = spawnSync(BINARY, args, {
    cwd: PROFILE,
    env: ENV,
    encoding: "utf8",
    timeout: 60_000,
  });
  /* A portable copy announces its data directory on stderr before the
   * headless plane writes anything; the installed app prints neither line.
   * Everything else on stderr counts. */
  const stderr = result.stderr.replace(
    /^\[portable\] (data dir|Hugging Face home): .*\n/gm,
    "",
  );
  return { code: result.status, stdout: result.stdout, stderr };
}

function json(text: string, what: string): Record<string, unknown> {
  try {
    return JSON.parse(text);
  } catch {
    fail(
      `${what} is not one JSON value: ${JSON.stringify(text.slice(0, 400))}`,
    );
  }
}

function git(...args: string[]): { code: number | null; out: string } {
  const result = spawnSync("git", args, { cwd: ROOT, encoding: "utf8" });
  return { code: result.status, out: result.stdout.trim() };
}

/* The build stamps the commit it was built from, plus `-dirty` for an
 * uncommitted edit under src-tauri (build.rs). The binary is stale when
 * src-tauri differs between that commit and HEAD, or when src-tauri has
 * uncommitted edits and the stamp does not say so: the build did not run,
 * or ran in another checkout. A change outside src-tauri compiles into
 * nothing here. */
function checkVersion(): void {
  const { code, stdout } = headless(["--version"]);
  const stamp = /\(([0-9a-f]{7,})(-dirty)?\)/.exec(stdout);
  const same =
    stamp !== null &&
    git("diff", "--quiet", stamp[1], "HEAD", "--", "src-tauri").code === 0;
  const edited = git("status", "--porcelain", "--", "src-tauri").out !== "";
  if (code !== 0 || !same || (edited && stamp?.[2] === undefined)) {
    fail(
      `--version printed ${JSON.stringify(stdout.trim())}; that build is not this checkout's src-tauri`,
    );
  }
  step(`version: ${stdout.trim()} (src-tauri matches HEAD)`);
}

/* With External access off the gate refuses before anything is opened. With
 * it on, the gate lets the request through to the corpus, which a portable
 * copy cannot open: it is encrypted with a key in the login keychain, and
 * portable mode turns native credential storage off. So `unavailable` is the
 * portable answer that proves the grant took effect. */
function checkMeetings(access: "off" | "on"): void {
  const expected = access === "off" ? "consent_required" : "unavailable";
  const run = headless(["--meetings", "--last", "1"]);
  const refusal = json(run.stderr, "--meetings stderr");
  if (run.code !== 1 || refusal.error !== expected || run.stdout !== "") {
    fail(
      `--meetings with access ${access}: exit ${run.code}, stdout ${JSON.stringify(run.stdout)}, ` +
        `stderr ${run.stderr.trim()}; expected exit 1 and ${expected}`,
    );
  }
  step(`--meetings with access ${access}: exit 1, ${expected}`);
}

class Core {
  private nextId = 1;
  private buffer = "";
  private waiting = new Map<number, (frame: Record<string, unknown>) => void>();

  private constructor(
    readonly child: ChildProcess,
    private readonly socket: Socket,
  ) {
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      this.buffer += chunk;
      let newline: number;
      while ((newline = this.buffer.indexOf("\n")) >= 0) {
        const line = this.buffer.slice(0, newline);
        this.buffer = this.buffer.slice(newline + 1);
        const frame = JSON.parse(line);
        // Event frames carry no id; only replies are awaited.
        if (typeof frame.id === "number") this.waiting.get(frame.id)?.(frame);
      }
    });
  }

  static async launch(): Promise<Core> {
    const log = Bun.file(CORE_LOG).writer();
    const child = spawn(BINARY, ["--native-socket", SOCKET], {
      cwd: PROFILE,
      env: ENV,
      stdio: ["ignore", "pipe", "pipe"],
    });
    child.stdout?.on("data", (chunk) => log.write(chunk));
    child.stderr?.on("data", (chunk) => log.write(chunk));
    child.on("exit", () => log.end());
    step(`core pid ${child.pid}, waiting for its socket`);
    try {
      /* The socket is bound only after every manager is registered, so its
       * appearance is the core's own "ready". */
      const deadline = Date.now() + 120_000;
      while (!existsSync(SOCKET)) {
        if (child.exitCode !== null)
          fail(
            `core exited with ${child.exitCode} before binding; see ${CORE_LOG}`,
          );
        if (Date.now() > deadline)
          fail(`core did not bind ${SOCKET} within 120 s; see ${CORE_LOG}`);
        await Bun.sleep(250);
      }
      const connected = Promise.withResolvers<Socket>();
      const connection = createConnection(SOCKET, () =>
        connected.resolve(connection),
      );
      connection.once("error", connected.reject);
      const socket = await connected.promise;
      return new Core(child, socket);
    } catch (error) {
      // process.exit does not take the child with it.
      if (child.exitCode === null && child.signalCode === null) {
        const gone = Promise.withResolvers<void>();
        child.once("exit", () => gone.resolve());
        child.kill("SIGKILL");
        await gone.promise;
        step(`killed core pid ${child.pid}`);
      }
      throw error;
    }
  }

  call(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
    const id = this.nextId++;
    const reply = Promise.withResolvers<unknown>();
    const timer = setTimeout(
      () => reply.reject(new Error(`${method}: no reply in 30 s`)),
      30_000,
    );
    this.waiting.set(id, (frame) => {
      clearTimeout(timer);
      this.waiting.delete(id);
      if ("error" in frame)
        reply.reject(new Error(`${method}: ${JSON.stringify(frame.error)}`));
      else reply.resolve(frame.result);
    });
    this.socket.write(`${JSON.stringify({ id, method, params })}\n`);
    return reply.promise;
  }

  async shutdown(): Promise<void> {
    const exited = Promise.withResolvers<number | null | "timeout">();
    if (this.child.exitCode !== null) exited.resolve(this.child.exitCode);
    else this.child.once("exit", (code) => exited.resolve(code));
    // The core may exit before it replies, so the reply is not awaited.
    this.socket.write(
      `${JSON.stringify({ id: this.nextId++, method: "shutdown" })}\n`,
    );
    const timer = setTimeout(() => exited.resolve("timeout"), 20_000);
    const code = await exited.promise;
    clearTimeout(timer);
    this.socket.destroy();
    if (code !== 0) fail(`core did not exit cleanly after shutdown: ${code}`);
    step("core exited 0 after shutdown");
  }
}

async function driveCore(): Promise<void> {
  const core = await Core.launch();
  try {
    if ((await core.call("is_portable")) !== true)
      fail("core does not report portable mode");
    const before = (await core.call("get_app_settings")) as Record<
      string,
      unknown
    >;
    if (
      before.external_query_enabled !== false ||
      before.detection_enabled !== false
    ) {
      fail(
        `unexpected starting settings: external_query_enabled=${before.external_query_enabled} detection_enabled=${before.detection_enabled}`,
      );
    }
    await core.call("change_external_query_enabled_setting", { enabled: true });
    const after = (await core.call("get_app_settings")) as Record<
      string,
      unknown
    >;
    if (after.external_query_enabled !== true)
      fail("External access did not turn on");
    step("over the socket: is_portable=true, External access turned on");
    await core.shutdown();
  } finally {
    if (core.child.exitCode === null && core.child.signalCode === null) {
      core.child.kill("SIGKILL");
      step(`killed core pid ${core.child.pid}`);
    }
  }
}

function checkNothingLeft(): void {
  const left = spawnSync("pgrep", ["-f", BINARY], {
    encoding: "utf8",
  }).stdout.trim();
  if (left) fail(`processes still running from ${BINARY}: ${left}`);
  const log = readFileSync(CORE_LOG, "utf8");
  if (/panicked at/.test(log)) fail(`the core panicked; see ${CORE_LOG}`);
  step("no process left from the profile; no panic in the core log");
}

async function main(): Promise<void> {
  if (!process.argv.includes("--no-build")) build();
  freshProfile();
  checkVersion();
  checkMeetings("off");
  await driveCore();
  checkMeetings("on");
  checkNothingLeft();
  step("PASS");
}

main().catch((error: unknown) => {
  console.error(
    `[verify] FAIL: ${error instanceof Error ? error.message : String(error)}`,
  );
  process.exit(1);
});
