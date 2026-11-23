/* Dictation accuracy and speed eval.
 *
 * `bun run eval:dictation` runs each local model through the engine's own
 * headless path (`sona --transcribe-file`) over two sets and prints one JSON
 * value: word error rate, median and p95 latency, and real-time factor per
 * model and set.
 *
 *   say          Synthetic clips spoken by the macOS system voice from the
 *                references in `dictation-eval/synthetic.json`: names,
 *                numbers, dates and punctuation.
 *   librispeech  The first 30 test-clean utterances (CC BY 4.0, OpenSLR 12),
 *                downloaded at run time.
 *
 * Audio, transcripts and models stay in the work directory (default
 * /private/var/tmp/sona-eval), never in the repository. The engine runs from
 * a portable profile there, so the developer's own settings and models are
 * never read or written. Models and the archive are free downloads checked
 * against pinned SHA-256 digests.
 *
 * Metrics. Each clip is a fresh process: the model loads once and decodes
 * `--repeat` times (default 3). A clip's latency is the median of its decodes;
 * a set's median and p95 (nearest rank) are over those per-clip medians. RTF
 * is total per-clip median decode time over total audio time, lower is
 * better. WER is corpus-weighted word edits over reference words after NFKC,
 * lowercasing and replacing punctuation and symbols with spaces, with no
 * number verbalization: "27" against "twenty seven" counts as errors. For the
 * synthetic set `written_wer_percent` scores against each clip's written form
 * ("$427.50") where it has one, which is what dictation should paste. */
import { createHash } from "node:crypto";
import {
  copyFileSync,
  createReadStream,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { z } from "zod";

import { resolveCargoTargetDirectory } from "./cargo-target-dir";

const ROOT = resolve(import.meta.dirname, "..");
const SYNTHETIC_PATH = resolve(
  import.meta.dirname,
  "dictation-eval",
  "synthetic.json",
);
const DEFAULT_MODELS = [
  "parakeet-tdt-0.6b-v2",
  "parakeet-tdt-0.6b-v3",
  "small",
];
const DEFAULT_SETS = ["say", "librispeech"];
const DEFAULT_WORK_DIRECTORY = "/private/var/tmp/sona-eval";
const LABEL = "[dictation-eval]";

const LIBRISPEECH_ARCHIVE = "test-clean.tar.gz";
const LIBRISPEECH_SHA256 =
  "39fde525e59672dc6d1551919b1478f724438a95aa55f874b576be21967e6c23";
/* openslr.org itself often serves this archive at about 100 KB/s; the
 * official mirrors carry the same bytes, which the digest check confirms. */
const LIBRISPEECH_URLS = [
  "https://openslr.elda.org/resources/12/test-clean.tar.gz",
  "https://openslr.trmal.net/resources/12/test-clean.tar.gz",
  "https://www.openslr.org/resources/12/test-clean.tar.gz",
];

export interface Clip {
  set: string;
  id: string;
  wav: string;
  reference: string;
  written: string;
}

export interface ClipResult {
  clip: Clip;
  audioSeconds: number;
  loadMs: number;
  decodeMs: number[];
  backend: string | null;
  hypothesis: string;
}

const syntheticSchema = z.object({
  voice: z.string(),
  rate: z.number().int().positive(),
  clips: z.array(
    z.object({
      id: z.string(),
      text: z.string(),
      written: z.string().optional(),
    }),
  ),
});

const engineOutputSchema = z.object({
  model: z.string(),
  bound_backend: z.string().nullable(),
  audio_secs: z.number(),
  load_ms: z.number(),
  transcribe_ms: z.array(z.number()),
  text: z.string(),
});

const modelListSchema = z.array(
  z.object({
    id: z.string(),
    filename: z.string(),
    is_directory: z.boolean(),
    is_downloaded: z.boolean(),
    source: z.unknown(),
  }),
);
const urlSourceSchema = z.object({
  Url: z.object({ url: z.string(), sha256: z.string().nullable() }),
});
type ListedModel = z.infer<typeof modelListSchema>[number];

/** Lowercased words with punctuation and symbols removed, as the WER counts them. */
export function normalizeWords(text: string): string[] {
  return text
    .normalize("NFKC")
    .toLowerCase()
    .replace(/[\p{P}\p{S}]/gu, " ")
    .split(/\s+/)
    .filter((word) => word.length > 0);
}

/** Word-level Levenshtein distance: substitutions, insertions and deletions. */
export function wordEdits(reference: string[], hypothesis: string[]): number {
  let previous = Array.from(
    { length: hypothesis.length + 1 },
    (_, index) => index,
  );
  for (let row = 1; row <= reference.length; row += 1) {
    const current = [row];
    for (let column = 1; column <= hypothesis.length; column += 1) {
      const substitution =
        previous[column - 1] +
        (reference[row - 1] === hypothesis[column - 1] ? 0 : 1);
      current.push(
        Math.min(substitution, previous[column] + 1, current[column - 1] + 1),
      );
    }
    previous = current;
  }
  return previous[hypothesis.length];
}

export function median(values: number[]): number {
  if (values.length === 0) {
    return Number.NaN;
  }
  const sorted = [...values].sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
}

/** Nearest-rank percentile: the smallest value with at least `percent`% at or below it. */
export function percentile(values: number[], percent: number): number {
  if (values.length === 0) {
    return Number.NaN;
  }
  const sorted = [...values].sort((left, right) => left - right);
  const rank = Math.max(1, Math.ceil((percent / 100) * sorted.length));
  return sorted[rank - 1];
}

function round(value: number, digits: number): number {
  const scale = 10 ** digits;
  return Math.round(value * scale) / scale;
}

export function summarizeSet(results: ClipResult[]) {
  let referenceWords = 0;
  let edits = 0;
  let writtenWords = 0;
  let writtenEdits = 0;
  let audioSeconds = 0;
  let decodeSeconds = 0;
  const latencies: number[] = [];
  for (const result of results) {
    const hypothesis = normalizeWords(result.hypothesis);
    const reference = normalizeWords(result.clip.reference);
    const written = normalizeWords(result.clip.written);
    referenceWords += reference.length;
    edits += wordEdits(reference, hypothesis);
    writtenWords += written.length;
    writtenEdits += wordEdits(written, hypothesis);
    const latency = median(result.decodeMs);
    latencies.push(latency);
    audioSeconds += result.audioSeconds;
    decodeSeconds += latency / 1000;
  }
  return {
    clips: results.length,
    reference_words: referenceWords,
    word_edits: edits,
    wer_percent: round((100 * edits) / referenceWords, 2),
    written_wer_percent: round((100 * writtenEdits) / writtenWords, 2),
    latency_ms: {
      median: round(median(latencies), 1),
      p95: percentile(latencies, 95),
    },
    rtf: round(decodeSeconds / audioSeconds, 4),
    load_ms_median: round(median(results.map((result) => result.loadMs)), 1),
    audio_seconds: round(audioSeconds, 2),
  };
}

function log(message: string): void {
  console.error(`${LABEL} ${message}`);
}

function run(command: string[], cwd: string = ROOT): string {
  const result = Bun.spawnSync(command, {
    cwd,
    stdout: "pipe",
    stderr: "pipe",
  });
  if (result.exitCode !== 0) {
    throw new Error(
      `${command.join(" ")} exited ${result.exitCode}: ${result.stderr.toString().trim()}`,
    );
  }
  return result.stdout.toString();
}

async function sha256(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) {
    hash.update(chunk);
  }
  return hash.digest("hex");
}

/** Downloads `urls[i]` until one yields a file with the pinned digest. */
async function download(
  urls: string[],
  destination: string,
  expectedSha256: string,
): Promise<void> {
  if (
    existsSync(destination) &&
    (await sha256(destination)) === expectedSha256
  ) {
    return;
  }
  const partial = `${destination}.partial`;
  for (const url of urls) {
    log(`downloading ${url}`);
    const result = Bun.spawnSync(
      ["curl", "-fL", "--retry", "2", "-C", "-", "-sS", url, "-o", partial],
      { stdout: "inherit", stderr: "inherit" },
    );
    if (result.exitCode === 0 && (await sha256(partial)) === expectedSha256) {
      renameSync(partial, destination);
      return;
    }
    rmSync(partial, { force: true });
  }
  throw new Error(
    `could not download ${destination} with SHA-256 ${expectedSha256}`,
  );
}

function defaultBinary(): string {
  const target =
    resolveCargoTargetDirectory() ?? join(ROOT, "src-tauri", "target");
  return join(target, "release", "sona");
}

/** A throwaway portable profile holding a copy of the engine, so no run touches real settings. */
function prepareProfile(binary: string, workDirectory: string): string {
  if (!existsSync(binary)) {
    throw new Error(
      `no engine at ${binary}; build it with \`bun run build\` or pass --binary`,
    );
  }
  const profile = join(workDirectory, "profile");
  mkdirSync(join(profile, "Data", "models"), { recursive: true });
  for (const directory of [workDirectory, profile]) {
    if (statSync(directory).uid !== process.getuid?.()) {
      throw new Error(`${directory} belongs to another account; pass --work-dir`);
    }
  }
  copyFileSync(binary, join(profile, "sona"));
  writeFileSync(join(profile, "portable"), "Sona Portable Mode");
  return profile;
}

function engineEnvironment() {
  return { ...process.env, SONA_ALLOW_PORTABLE_DEV: "1" };
}

function listModels(profile: string): ListedModel[] {
  const result = Bun.spawnSync(
    [join(profile, "sona"), "--list-models", "--json"],
    {
      cwd: profile,
      env: engineEnvironment(),
      stdout: "pipe",
      stderr: "pipe",
    },
  );
  if (result.exitCode !== 0) {
    throw new Error(
      `sona --list-models failed: ${result.stderr.toString().trim()}`,
    );
  }
  return modelListSchema.parse(JSON.parse(result.stdout.toString()));
}

async function installModels(
  profile: string,
  workDirectory: string,
  modelIds: string[],
): Promise<void> {
  const listed = listModels(profile);
  const modelsDirectory = join(profile, "Data", "models");
  for (const id of modelIds) {
    const model = listed.find((candidate) => candidate.id === id);
    if (!model) {
      throw new Error(`unknown model ${id}; see sona --list-models`);
    }
    if (model.is_downloaded) {
      continue;
    }
    const source = urlSourceSchema.safeParse(model.source).data?.Url;
    if (!source?.sha256) {
      throw new Error(`model ${id} has no pinned URL download`);
    }
    const archive = join(
      workDirectory,
      "downloads",
      source.url.split("/").pop() ?? id,
    );
    mkdirSync(join(workDirectory, "downloads"), { recursive: true });
    await download([source.url], archive, source.sha256);
    if (model.is_directory) {
      run(["tar", "-xzf", archive, "-C", modelsDirectory]);
    } else {
      copyFileSync(archive, join(modelsDirectory, model.filename));
    }
    log(`installed ${id}`);
  }
  const missing = listModels(profile).filter(
    (model) => modelIds.includes(model.id) && !model.is_downloaded,
  );
  if (missing.length > 0) {
    throw new Error(
      `engine does not see ${missing.map((model) => model.id).join(", ")} as installed`,
    );
  }
}

function syntheticClips(workDirectory: string): Clip[] {
  const fixtures = syntheticSchema.parse(
    JSON.parse(readFileSync(SYNTHETIC_PATH, "utf8")),
  );
  const directory = join(workDirectory, "say");
  mkdirSync(directory, { recursive: true });
  const clips: Clip[] = [];
  for (const fixture of fixtures.clips) {
    const wav = join(directory, `${fixture.id}.wav`);
    const textPath = join(directory, `${fixture.id}.txt`);
    const spoken = `${fixtures.voice} ${fixtures.rate} ${fixture.text}`;
    const stale =
      !existsSync(wav) ||
      !existsSync(textPath) ||
      readFileSync(textPath, "utf8") !== spoken;
    if (stale) {
      run([
        "say",
        "-v",
        fixtures.voice,
        "-r",
        String(fixtures.rate),
        "--file-format=WAVE",
        "--data-format=LEI16@16000",
        "-o",
        wav,
        fixture.text,
      ]);
      writeFileSync(textPath, spoken);
    }
    clips.push({
      set: "say",
      id: fixture.id,
      wav,
      reference: fixture.text,
      written: fixture.written ?? fixture.text,
    });
  }
  return clips;
}

async function librispeechClips(
  workDirectory: string,
  count: number,
): Promise<Clip[]> {
  const downloads = join(workDirectory, "downloads");
  mkdirSync(downloads, { recursive: true });
  const archive = join(downloads, LIBRISPEECH_ARCHIVE);
  await download(LIBRISPEECH_URLS, archive, LIBRISPEECH_SHA256);
  const directory = join(workDirectory, "librispeech");
  mkdirSync(directory, { recursive: true });
  const members = run(["tar", "-tzf", archive]).split("\n");
  const flacs = members
    .filter((member) => member.endsWith(".flac"))
    .sort()
    .slice(0, count);
  const transcripts = [
    ...new Set(
      flacs.map((member) => member.replace(/-\d+\.flac$/, ".trans.txt")),
    ),
  ];
  const needed = [...flacs, ...transcripts].filter(
    (member) => !existsSync(join(directory, member)),
  );
  if (needed.length > 0) {
    run(["tar", "-xzf", archive, "-C", directory, ...needed]);
  }
  const references = new Map<string, string>();
  for (const transcript of transcripts) {
    for (const line of readFileSync(join(directory, transcript), "utf8").split(
      "\n",
    )) {
      const space = line.indexOf(" ");
      if (space > 0) {
        references.set(line.slice(0, space), line.slice(space + 1).trim());
      }
    }
  }
  return flacs.map((member) => {
    const id =
      member
        .split("/")
        .pop()
        ?.replace(/\.flac$/, "") ?? member;
    const wav = join(directory, `${id}.wav`);
    if (!existsSync(wav)) {
      run([
        "afconvert",
        "-f",
        "WAVE",
        "-d",
        "LEI16@16000",
        "-c",
        "1",
        join(directory, member),
        wav,
      ]);
    }
    const reference = references.get(id);
    if (reference === undefined) {
      throw new Error(`no reference for ${id}`);
    }
    return { set: "librispeech", id, wav, reference, written: reference };
  });
}

function transcribe(
  profile: string,
  model: string,
  clip: Clip,
  repeat: number,
): ClipResult {
  const result = Bun.spawnSync(
    [
      join(profile, "sona"),
      "--transcribe-file",
      clip.wav,
      "--model",
      model,
      "--repeat",
      String(repeat),
      "--json",
    ],
    { cwd: profile, env: engineEnvironment(), stdout: "pipe", stderr: "pipe" },
  );
  if (result.exitCode !== 0) {
    throw new Error(
      `${model} on ${clip.id} exited ${result.exitCode}: ${result.stderr.toString().trim()}`,
    );
  }
  const line = result.stdout
    .toString()
    .split("\n")
    .findLast((candidate) => candidate.startsWith("{"));
  if (line === undefined) {
    throw new Error(`${model} on ${clip.id} printed no JSON`);
  }
  const output = engineOutputSchema.parse(JSON.parse(line));
  return {
    clip,
    audioSeconds: output.audio_secs,
    loadMs: output.load_ms,
    decodeMs: output.transcribe_ms,
    backend: output.bound_backend,
    hypothesis: output.text,
  };
}

function machine() {
  const sysctl = (name: string) => run(["sysctl", "-n", name]).trim();
  return {
    hardware: sysctl("hw.model"),
    cpu: sysctl("machdep.cpu.brand_string"),
    cpus: Number(sysctl("hw.ncpu")),
    memory_gib: round(Number(sysctl("hw.memsize")) / 2 ** 30, 1),
    macos: run(["sw_vers", "-productVersion"]).trim(),
  };
}

function commit(): string {
  const head = run(["git", "rev-parse", "--short=12", "HEAD"]).trim();
  const dirty =
    run(["git", "status", "--porcelain", "--untracked-files=no"]).trim()
      .length > 0;
  return dirty ? `${head}-dirty` : head;
}

function parseList(value: string | undefined, fallback: string[]): string[] {
  return value
    ? value
        .split(",")
        .map((item) => item.trim())
        .filter((item) => item.length > 0)
    : fallback;
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      models: { type: "string" },
      sets: { type: "string" },
      repeat: { type: "string", default: "3" },
      binary: { type: "string" },
      "work-dir": { type: "string", default: DEFAULT_WORK_DIRECTORY },
      "librispeech-count": { type: "string", default: "30" },
    },
  });
  const models = parseList(values.models, DEFAULT_MODELS);
  const sets = parseList(values.sets, DEFAULT_SETS);
  const repeat = Number(values.repeat);
  const librispeechCount = Number(values["librispeech-count"]);
  if (
    !Number.isInteger(repeat) ||
    repeat < 1 ||
    !Number.isInteger(librispeechCount) ||
    librispeechCount < 1
  ) {
    throw new Error("--repeat and --librispeech-count take positive integers");
  }
  const unknownSets = sets.filter((set) => !DEFAULT_SETS.includes(set));
  if (unknownSets.length > 0) {
    throw new Error(
      `unknown set ${unknownSets.join(", ")}; choose from ${DEFAULT_SETS.join(", ")}`,
    );
  }
  const workDirectory = resolve(values["work-dir"]);
  const binary = resolve(values.binary ?? defaultBinary());

  const profile = prepareProfile(binary, workDirectory);
  await installModels(profile, workDirectory, models);
  const clips: Clip[] = [];
  if (sets.includes("say")) {
    clips.push(...syntheticClips(workDirectory));
  }
  if (sets.includes("librispeech")) {
    clips.push(...(await librispeechClips(workDirectory, librispeechCount)));
  }

  const perClip: { model: string; result: ClipResult }[] = [];
  const results = [];
  for (const model of models) {
    const sections = [];
    const modelResults: ClipResult[] = [];
    for (const set of sets) {
      const setResults = clips
        .filter((clip) => clip.set === set)
        .map((clip, index, all) => {
          log(`${model} ${set} ${index + 1}/${all.length} ${clip.id}`);
          return transcribe(profile, model, clip, repeat);
        });
      modelResults.push(...setResults);
      sections.push({ set, ...summarizeSet(setResults) });
    }
    perClip.push(...modelResults.map((result) => ({ model, result })));
    const backend =
      modelResults.find((result) => result.backend !== null)?.backend ?? null;
    results.push({ model, backend, sets: sections });
  }

  const detailsPath = join(workDirectory, "last-run-clips.json");
  writeFileSync(
    detailsPath,
    JSON.stringify(
      perClip.map(({ model, result }) => ({
        model,
        set: result.clip.set,
        id: result.clip.id,
        reference: result.clip.reference,
        hypothesis: result.hypothesis,
        decode_ms: result.decodeMs,
        load_ms: result.loadMs,
        audio_seconds: result.audioSeconds,
      })),
      null,
      2,
    ),
  );
  log(`per-clip transcripts in ${detailsPath}`);
  console.log(
    JSON.stringify(
      { commit: commit(), machine: machine(), repeat, results },
      null,
      2,
    ),
  );
}

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`${LABEL} ${message}`);
    process.exitCode = 1;
  }
}
