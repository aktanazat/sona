/* Records a live test of the Mac app with each step labeled on the video.
 * `start` launches a detached supervisor that owns ffmpeg, `note` stamps a
 * label with the wall clock, and `stop` asks the supervisor to end the
 * capture, then burns the labels in and writes report.md with one frame per
 * check. Every wait is on a file the other side writes, never on a timer. */
import { spawn, spawnSync } from "node:child_process";
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  watch,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";

const HERE = import.meta.dirname;
const SCRIPT = import.meta.filename;
const ROOT = resolve(HERE, "../../..");
const WIDTH = 1920;
const MAX_LABEL = 80;
const RESULTS = ["passed", "failed", "untested"] as const;

type Result = (typeof RESULTS)[number];
type Note =
  | { at: number; kind: "setup" | "test"; text: string }
  | { at: number; kind: "check"; text: string; result: Result };
type Session = {
  title: string;
  screen: string;
  commit: string;
  branch: string;
  dirty: boolean;
  startedAt: string;
};
type RecorderExit = {
  code: number | null;
  signal: string | null;
  origin: number | null;
};
type Banner = {
  test: string;
  check: string;
  result: string;
  from: number;
  to: number;
};

function fail(message: string): never {
  console.error(`[record] FAIL: ${message}`);
  process.exit(1);
}

function flag(name: string): string | undefined {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? undefined : process.argv[index + 1];
}

function run(command: string, args: string[]): string {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: "utf8" });
  if (result.status !== 0)
    fail(
      `${command} ${args.join(" ")} exited ${result.status}: ${result.stderr.trim()}`,
    );
  return result.stdout.trim();
}

function waitForFile(dir: string, names: string[]): Promise<string> {
  const { promise, resolve: done } = Promise.withResolvers<string>();
  const watcher = watch(dir, () => check());
  function check(): void {
    const hit = names.find((name) => existsSync(join(dir, name)));
    if (hit === undefined) return;
    watcher.close();
    done(hit);
  }
  check();
  return promise;
}

function readSession(dir: string): Session {
  const path = join(dir, "session.json");
  if (!existsSync(path)) fail(`${dir} is not a recording session`);
  return JSON.parse(readFileSync(path, "utf8")) as Session;
}

function readNotes(dir: string): Note[] {
  const path = join(dir, "notes.jsonl");
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8")
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => JSON.parse(line) as Note);
}

function duration(path: string): number {
  const seconds = Number(
    run("ffprobe", [
      "-v",
      "error",
      "-show_entries",
      "format=duration",
      "-of",
      "csv=p=0",
      path,
    ]),
  );
  if (!(seconds > 0)) fail(`${path} has no playable video`);
  return seconds;
}

function clock(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${(seconds - minutes * 60).toFixed(1).padStart(4, "0")}`;
}

/* Stills come from screencapture: a second avfoundation capture blocks while a
 * recording holds the screen. Its -D counts displays from 1 in the order
 * avfoundation numbers "Capture screen 0, 1, ...". */
function screens(): void {
  const listing = spawnSync(
    "ffmpeg",
    ["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
    { encoding: "utf8" },
  ).stderr;
  const devices = [...listing.matchAll(/\[(\d+)\] Capture screen (\d+)/g)];
  if (devices.length === 0) fail(`ffmpeg lists no screens:\n${listing}`);
  const dir = "/private/var/tmp/sona-evidence/screens";
  mkdirSync(dir, { recursive: true });
  for (const [, device, screen] of devices) {
    const png = join(dir, `screen-${device}.png`);
    run("screencapture", ["-x", "-D", String(Number(screen) + 1), png]);
    console.log(`--screen ${device}: ${png}`);
  }
}

async function start(): Promise<void> {
  const title = flag("title") ?? fail("--title is required");
  const screen =
    flag("screen") ??
    fail("--screen is required; `screens` shows which number is which display");
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const dir = resolve(flag("out") ?? `/private/var/tmp/sona-evidence/${stamp}`);
  if (existsSync(join(dir, "session.json")))
    fail(`${dir} already holds a session`);
  mkdirSync(dir, { recursive: true });
  const session: Session = {
    title,
    screen,
    commit: run("git", ["rev-parse", "HEAD"]),
    branch: run("git", ["branch", "--show-current"]),
    dirty: run("git", ["status", "--porcelain"]) !== "",
    startedAt: new Date().toISOString(),
  };
  writeFileSync(join(dir, "session.json"), JSON.stringify(session, null, 2));
  const supervisor = spawn(process.execPath, [SCRIPT, "supervise", dir], {
    detached: true,
    stdio: ["ignore", "ignore", openSync(join(dir, "ffmpeg.log"), "w")],
  });
  supervisor.unref();
  writeFileSync(join(dir, "supervisor.pid"), String(supervisor.pid));
  if ((await waitForFile(dir, ["origin", "recorder.exit"])) === "recorder.exit")
    fail(
      `ffmpeg stopped before the first frame; see ${join(dir, "ffmpeg.log")}`,
    );
  console.log(`[record] recording; SESSION=${dir}`);
}

/* Runs detached. The video's time zero is the earliest wall-clock estimate
 * (now minus encoded time) across ffmpeg's progress reports: every report
 * arrives late, never early, so the smallest one is the closest. */
function supervise(dir: string): void {
  const session = readSession(dir);
  const ffmpeg = spawn(
    "ffmpeg",
    [
      "-hide_banner",
      "-nostats",
      "-loglevel",
      "error",
      "-progress",
      "pipe:1",
      "-f",
      "avfoundation",
      "-pixel_format",
      "nv12",
      "-capture_cursor",
      "1",
      "-framerate",
      "30",
      "-i",
      `${session.screen}:none`,
      "-c:v",
      "h264_videotoolbox",
      "-realtime",
      "1",
      "-b:v",
      "12M",
      "-f",
      "mpegts",
      join(dir, "raw.ts"),
    ],
    { stdio: ["pipe", "pipe", "inherit"] },
  );
  let origin: number | null = null;
  let frame = 0;
  createInterface({ input: ffmpeg.stdout }).on("line", (line) => {
    const [key, value] = line.split("=");
    if (key === "frame") frame = Number(value);
    if (key !== "out_time_us" || frame === 0 || !/^\d+$/.test(value)) return;
    const estimate = Date.now() / 1000 - Number(value) / 1e6;
    if (origin !== null && estimate >= origin) return;
    origin = estimate;
    writeFileSync(join(dir, "origin"), String(origin));
  });
  const watcher = watch(dir, () => {
    if (existsSync(join(dir, "stop.request")) && ffmpeg.stdin.writable)
      ffmpeg.stdin.end("q");
  });
  ffmpeg.on("exit", (code, signal) => {
    watcher.close();
    const exit: RecorderExit = { code, signal, origin };
    writeFileSync(join(dir, "recorder.exit"), JSON.stringify(exit));
  });
}

function note(dir: string): void {
  readSession(dir);
  if (!existsSync(join(dir, "origin"))) fail("the recording has not started");
  if (
    existsSync(join(dir, "recorder.exit")) ||
    existsSync(join(dir, "stop.request"))
  )
    fail("the recording has stopped");
  const at = Date.now() / 1000;
  const setup = flag("setup");
  const test = flag("test");
  const check = flag("check");
  const entry: Note =
    setup !== undefined
      ? { at, kind: "setup", text: setup }
      : test !== undefined
        ? { at, kind: "test", text: test }
        : check !== undefined
          ? {
              at,
              kind: "check",
              text: check,
              result: parseResult(flag("result")),
            }
          : fail("pass --setup, --test or --check");
  if (entry.text.length > MAX_LABEL)
    fail(`labels must fit in ${MAX_LABEL} characters`);
  appendFileSync(join(dir, "notes.jsonl"), `${JSON.stringify(entry)}\n`);
  console.log(`[record] ${entry.kind} noted`);
}

function parseResult(value: string | undefined): Result {
  const result = RESULTS.find((candidate) => candidate === value);
  return result ?? fail(`--result must be one of ${RESULTS.join(", ")}`);
}

function supervisorAlive(dir: string): boolean {
  const pid = Number(readFileSync(join(dir, "supervisor.pid"), "utf8"));
  const command = spawnSync("ps", ["-o", "command=", "-p", String(pid)], {
    encoding: "utf8",
  });
  return command.status === 0 && command.stdout.includes(`supervise ${dir}`);
}

function banners(notes: Note[], origin: number, end: number): Banner[] {
  const result: Banner[] = [];
  let test = "";
  notes.forEach((entry, index) => {
    const from = Math.max(0, entry.at - origin);
    const next = notes[index + 1];
    const to = next === undefined ? end : Math.min(end, next.at - origin);
    if (entry.kind === "setup") test = `Setup: ${entry.text}`;
    if (entry.kind === "test") test = entry.text;
    const check =
      entry.kind === "check"
        ? `${entry.result.toUpperCase()}  ${entry.text}`
        : "";
    const outcome = entry.kind === "check" ? entry.result : "";
    if (to > from) result.push({ test, check, result: outcome, from, to });
  });
  return result;
}

async function stop(dir: string): Promise<void> {
  const session = readSession(dir);
  if (!existsSync(join(dir, "recorder.exit"))) {
    if (!supervisorAlive(dir))
      fail(
        "the recording supervisor is gone; stop any leftover `ffmpeg -f avfoundation` yourself, then rerun stop",
      );
    writeFileSync(join(dir, "stop.request"), "");
    await waitForFile(dir, ["recorder.exit"]);
  }
  const exit = JSON.parse(
    readFileSync(join(dir, "recorder.exit"), "utf8"),
  ) as RecorderExit;
  if (exit.origin === null)
    fail(`ffmpeg captured no frames; see ${join(dir, "ffmpeg.log")}`);
  const raw = join(dir, "raw.ts");
  const length = duration(raw);
  const notes = readNotes(dir);
  const shown = banners(notes, exit.origin, length);

  const labels = join(dir, "labels");
  mkdirSync(labels, { recursive: true });
  const swift = spawnSync("swift", [join(HERE, "label.swift"), labels], {
    input: JSON.stringify({ width: WIDTH, banners: shown }),
    encoding: "utf8",
  });
  if (swift.status !== 0) fail(`label.swift failed: ${swift.stderr}`);

  const inputs = ["-i", raw];
  const filters = [`[0:v]setpts=PTS-STARTPTS,scale=${WIDTH}:-2[v0]`];
  shown.forEach((banner, index) => {
    inputs.push(
      "-i",
      join(labels, `label-${String(index).padStart(3, "0")}.png`),
    );
    filters.push(
      `[v${index}][${index + 1}:v]overlay=0:H-h:enable='between(t,${banner.from.toFixed(3)},${banner.to.toFixed(3)})'[v${index + 1}]`,
    );
  });
  const video = join(dir, "evidence.mp4");
  run("ffmpeg", [
    "-hide_banner",
    "-loglevel",
    "error",
    ...inputs,
    "-filter_complex",
    filters.join(";"),
    "-map",
    `[v${shown.length}]`,
    "-c:v",
    "libx264",
    "-preset",
    "veryfast",
    "-crf",
    "26",
    "-pix_fmt",
    "yuv420p",
    "-movflags",
    "+faststart",
    "-y",
    video,
  ]);
  const videoLength = duration(video);

  const frames = join(dir, "frames");
  mkdirSync(frames, { recursive: true });
  const rows: string[] = [];
  const framePaths: string[] = [];
  notes.forEach((entry, index) => {
    const at = Math.max(0, entry.at - exit.origin!);
    const result = entry.kind === "check" ? entry.result : "";
    rows.push(
      `| ${clock(at)} | ${entry.kind} | ${entry.text.replaceAll("|", "\\|")} | ${result} |`,
    );
    if (entry.kind !== "check") return;
    const png = join(
      frames,
      `${String(index).padStart(2, "0")}-${entry.result}.png`,
    );
    const seek = Math.min(at + 0.5, videoLength - 0.1);
    run("ffmpeg", [
      "-hide_banner",
      "-loglevel",
      "error",
      "-ss",
      seek.toFixed(3),
      "-i",
      video,
      "-frames:v",
      "1",
      "-y",
      png,
    ]);
    framePaths.push(png);
  });
  const counts = Object.fromEntries(
    RESULTS.map((result) => [
      result,
      notes.filter((entry) => entry.kind === "check" && entry.result === result)
        .length,
    ]),
  );
  writeFileSync(
    join(dir, "report.md"),
    [
      `# ${session.title}`,
      "",
      `- Commit: ${session.commit}${session.dirty ? " with uncommitted edits" : ""} on ${session.branch || "a detached checkout"}`,
      `- Recorded: ${session.startedAt}, ${videoLength.toFixed(1)} s, screen ${session.screen}`,
      `- Checks: ${RESULTS.map((result) => `${counts[result]} ${result}`).join(", ")}`,
      "",
      "| Time | Step | What | Result |",
      "|---|---|---|---|",
      ...rows,
      "",
      "## Caveats",
      "",
    ].join("\n"),
  );
  console.log(
    JSON.stringify(
      { video, report: join(dir, "report.md"), frames: framePaths, ...counts },
      null,
      2,
    ),
  );
}

const [command, target] = process.argv.slice(2);
const session = target === undefined ? undefined : resolve(target);
if (command === "screens") screens();
else if (command === "start") await start();
else if (command === "supervise" && session) supervise(session);
else if (command === "note" && session) note(session);
else if (command === "stop" && session) await stop(session);
else
  fail(
    "usage: record.ts screens | start --title T --screen N [--out DIR] | note DIR (--setup T | --test T | --check T --result R) | stop DIR",
  );
