/* Running `sona`, and reporting what it said.
 *
 * The whole server is this: spawn the installed app with read-only flags, read
 * one JSON value off stdout, and hand a refusal back unchanged. No cache, no
 * state, no second copy of anything Sona knows.
 */

import { spawn } from "node:child_process";
import { z } from "zod";

export const DEFAULT_SONA_BIN =
  "/Applications/Sona.app/Contents/Helpers/SonaCore.app/Contents/MacOS/sona-core";

/** How long one read may take before it is abandoned, in milliseconds.
 *
 * A corpus read is a handful of SQLite queries, but the first one of a session
 * mounts the encrypted store — which reads the OS keychain, and can sit behind
 * a system prompt nobody is looking at. This is the boundary where that wait
 * would otherwise be forever, and an agent that never gets an answer cannot
 * tell a slow Mac from a hung one. */
export const TIMEOUT_MS = 30_000;

/* A refusal as `sona` prints it, and the parser that recognises one. Older
 * builds printed refusals without a version, which read as 0 — the same number
 * the refusals this file writes itself carry. */
const SONA_REFUSAL = z.object({
  schema_version: z.number().default(0),
  error: z.string(),
  message: z.string(),
  settings_path: z.string().optional(),
});

export type SonaRefusal = z.infer<typeof SONA_REFUSAL>;

/* One JSON value, as `sona` printed it. This server models nothing about the
 * corpus: it forwards the bytes Sona chose, so "JSON" is the only contract it
 * can name without inventing a second copy of Sona's schema. */
const SONA_ANSWER = z.json();

export type SonaAnswer = z.infer<typeof SONA_ANSWER>;

/** What `sona` said when it refused, carried without reinterpretation. */
export class SonaCliError extends Error {
  readonly code: string;
  readonly settingsPath: string | undefined;
  readonly exitCode: number | null;

  constructor(refusal: SonaRefusal, exitCode: number | null) {
    super(refusal.message);
    this.name = "SonaCliError";
    this.code = refusal.error;
    this.settingsPath = refusal.settings_path;
    this.exitCode = exitCode;
  }
}

export function sonaBinary(): string {
  const configured = process.env.SONA_BIN;
  return configured === undefined || configured === ""
    ? DEFAULT_SONA_BIN
    : configured;
}

/** Sona's own refusal if stderr holds one, and what stderr says otherwise.
 *
 * Read from the last line back: headless Sona sends its log lines to stderr
 * too, so the refusal is the last thing printed rather than the only thing.
 *
 * When there is no refusal to read — a clap usage error, which Sona answers
 * in clap's words rather than in its own JSON — the exit code is the only
 * thing it said about the *kind* of failure, and it is a documented signal
 * rather than a guess. `ExternalErrorCode::exit_code` reserves 2 for bad
 * input and 1 for everything else, and clap exits 2 for a usage error for the
 * same reason. Reading it matters because the code chooses the JSON-RPC error
 * an agent sees: `failed` lands in `InternalError`, which says stop and
 * report, and a caller that misspelled an enum value or left out a paired
 * flag can fix that itself. */
function refusalFrom(stderr: string, exitCode: number | null): SonaRefusal {
  const lines = stderr.trim().split("\n");
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const line = lines[index]?.trim();
    if (line === undefined || !line.startsWith("{")) continue;
    try {
      const parsed = SONA_REFUSAL.safeParse(JSON.parse(line));
      if (parsed.success) return parsed.data;
    } catch {
      // A line that does not parse is one of those log lines, not an answer.
    }
  }
  return {
    schema_version: 0,
    error: exitCode === 2 ? "invalid_request" : "failed",
    message:
      stderr.trim() === ""
        ? `sona exited with code ${exitCode ?? "unknown"} and said nothing.`
        : stderr.trim(),
  };
}

/** One read: whatever JSON `sona` printed, or its refusal thrown. */
export function runSona(argv: readonly string[]): Promise<SonaAnswer> {
  const { promise, resolve, reject } = Promise.withResolvers<SonaAnswer>();
  const binary = sonaBinary();
  const child = spawn(binary, [...argv], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  const abandon = setTimeout(() => child.kill("SIGKILL"), TIMEOUT_MS);

  child.stdout.on("data", (chunk: Buffer) => {
    stdout += chunk.toString();
  });
  child.stderr.on("data", (chunk: Buffer) => {
    stderr += chunk.toString();
  });
  child.on("error", (error: NodeJS.ErrnoException) => {
    clearTimeout(abandon);
    const missing = error.code === "ENOENT";
    reject(
      new SonaCliError(
        {
          schema_version: 0,
          error: missing ? "not_installed" : "failed",
          message: missing
            ? `No Sona binary at ${binary}. Install Sona, or point SONA_BIN at it.`
            : `Sona could not be run: ${error.message}`,
        },
        null,
      ),
    );
  });
  child.on("close", (exitCode, signal) => {
    clearTimeout(abandon);
    if (signal === "SIGKILL") {
      reject(
        new SonaCliError(
          {
            schema_version: 0,
            error: "timed_out",
            message: `Sona did not answer within ${TIMEOUT_MS / 1000}s. Its meeting storage may be waiting on a keychain prompt.`,
          },
          null,
        ),
      );
      return;
    }
    if (exitCode !== 0) {
      reject(new SonaCliError(refusalFrom(stderr, exitCode), exitCode));
      return;
    }
    try {
      resolve(SONA_ANSWER.parse(JSON.parse(stdout)));
    } catch {
      reject(
        new SonaCliError(
          {
            schema_version: 0,
            error: "failed",
            message: `Sona printed something that is not JSON: ${stdout.trim().slice(0, 200)}`,
          },
          exitCode,
        ),
      );
    }
  });
  return promise;
}
