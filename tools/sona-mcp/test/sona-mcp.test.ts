import { afterEach, describe, expect, test } from "bun:test";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { ErrorCode, McpError } from "@modelcontextprotocol/sdk/types.js";
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runSona, SonaCliError } from "../src/cli.ts";
import { SonaInputError, TOOLS } from "../src/tools.ts";
import { createServer } from "../src/index.ts";

const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { force: true, recursive: true });
  }
  delete process.env.SONA_BIN;
});

interface Stub {
  /** The argv the last run passed, one argument per line. */
  argv(): string[];
}

/** A `sona` that records its argv and answers with `reply`.
 *
 * The replies are files the script cats rather than strings it prints, so a
 * multi-line stderr — Sona logging beside its refusal — arrives byte for byte
 * instead of through `sh`'s idea of an escape.
 */
function stub(reply: {
  stdout?: string;
  stderr?: string;
  exit?: number;
}): Stub {
  const directory = mkdtempSync(join(tmpdir(), "sona-mcp-"));
  directories.push(directory);
  const binary = join(directory, "sona");
  const recorded = join(directory, "argv");
  const script = [
    "#!/bin/sh",
    `: > ${JSON.stringify(recorded)}`,
    `for argument in "$@"; do printf '%s\\n' "$argument" >> ${JSON.stringify(recorded)}; done`,
  ];
  for (const [stream, body] of [
    ["stdout", reply.stdout],
    ["stderr", reply.stderr],
  ] as const) {
    if (body === undefined) continue;
    const path = join(directory, stream);
    writeFileSync(path, body);
    script.push(
      `cat ${JSON.stringify(path)}${stream === "stderr" ? " >&2" : ""}`,
    );
  }
  script.push(`exit ${reply.exit ?? 0}`, "");
  writeFileSync(binary, script.join("\n"));
  chmodSync(binary, 0o755);
  process.env.SONA_BIN = binary;
  return {
    argv: () =>
      readFileSync(recorded, "utf8")
        .split("\n")
        .filter((line) => line !== ""),
  };
}

function tool(name: string) {
  const found = TOOLS.find((candidate) => candidate.name === name);
  if (found === undefined) throw new Error(`no tool ${name}`);
  return found;
}

/** The refusal `sona` produced, or a failure saying it did not refuse. */
async function refusalOf(argv: readonly string[]): Promise<SonaCliError> {
  try {
    await runSona(argv);
  } catch (error) {
    if (error instanceof SonaCliError) return error;
    throw error;
  }
  throw new Error(`sona answered ${argv.join(" ")} instead of refusing`);
}

describe("tool arguments", () => {
  test("an argument that cannot become a command line is refused here", () => {
    expect(() => tool("sona_search").argv({})).toThrow(SonaInputError);
    expect(() => tool("sona_people").argv({ name: 42 })).toThrow(
      SonaInputError,
    );
    expect(() => tool("sona_search").argv({ query: "x", limit: 1.5 })).toThrow(
      SonaInputError,
    );
  });

  test("person stats are read by the exact person id", () => {
    const stats = tool("sona_person_stats");

    expect(stats.argv({ person_id: "2e2a5f0e-0000-4000-8000-000000000002" })).toEqual([
      "--person-stats",
      "2e2a5f0e-0000-4000-8000-000000000002",
    ]);
    expect(() => stats.argv({})).toThrow(SonaInputError);
  });
});

describe("running sona", () => {
  test("a refusal is found past the log lines printed beside it", async () => {
    stub({
      stderr: [
        "[2026-08-31][INFO][sona] meeting storage mounted",
        JSON.stringify({
          schema_version: 2,
          error: "unavailable",
          message: "The corpus is not open.",
        }),
      ].join("\n"),
      exit: 1,
    });

    const refused = await refusalOf(["--meetings"]);

    expect(refused.code).toBe("unavailable");
    expect(refused.settingsPath).toBeUndefined();
  });

  /* Clap answers a usage error in its own words rather than in JSON, so a
   * stderr that is not a refusal still has to reach the caller intact — and
   * its exit code still has to be read. 2 is bad input on both sides of this
   * boundary: `ExternalErrorCode::exit_code` reserves it, and clap uses it.
   * Measured against the shipped binary: `--scope nonsense` and `--from`
   * without `--to` both exit 2 with plain text, and both reached an agent as
   * `failed` -> InternalError, which says stop rather than fix your call. */
  test("a usage error sona had no JSON for is still the caller's to fix", async () => {
    stub({ stderr: "error: unexpected argument '--nope' found", exit: 2 });

    const refused = await refusalOf(["--nope"]);

    expect(refused.code).toBe("invalid_request");
    expect(refused.message).toContain("unexpected argument");
    expect(refused.exitCode).toBe(2);
  });

  /* And the discrimination is real rather than a relabelling of everything:
   * a non-JSON failure that is not exit 2 is still this side's problem. */
  test("a non-JSON failure that is not a usage error stays a failure", async () => {
    stub({ stderr: "dyld: Library not loaded", exit: 1 });

    const refused = await refusalOf(["--meetings"]);

    expect(refused.code).toBe("failed");
    expect(refused.message).toContain("Library not loaded");
  });

  test("a missing install says where it looked", async () => {
    process.env.SONA_BIN = join(tmpdir(), "sona-mcp-nonexistent", "sona");

    const refused = await refusalOf(["--meetings"]);

    expect(refused.code).toBe("not_installed");
    expect(refused.message).toContain("sona-mcp-nonexistent");
  });
});

async function connected(): Promise<Client> {
  const [clientEnd, serverEnd] = InMemoryTransport.createLinkedPair();
  const client = new Client({ name: "sona-mcp-test", version: "0.1.0" });
  await Promise.all([
    createServer().connect(serverEnd),
    client.connect(clientEnd),
  ]);
  return client;
}

/** What the SDK itself lets a caller hand to `tools/call` — the owner type,
 * so a deliberately malformed shape in these tests still speaks the wire's
 * own vocabulary rather than a local dictionary's. */
type RawCallArguments = NonNullable<
  Parameters<Client["callTool"]>[0]["arguments"]
>;

/** The error a `tools/call` came back as, or a failure saying it succeeded. */
async function errorOf(
  name: string,
  args: RawCallArguments,
): Promise<McpError> {
  const client = await connected();
  try {
    await client.callTool({ name, arguments: args });
  } catch (error) {
    if (error instanceof McpError) return error;
    throw error;
  } finally {
    await client.close();
  }
  throw new Error(`${name} answered instead of failing`);
}

describe("the stdio server", () => {
  /* The refusal an agent has to be able to branch on: the code and the
   * settings row survive the trip out through JSON-RPC. */
  test("a consent refusal arrives as an invalid-request error naming the switch", async () => {
    stub({
      stderr: JSON.stringify({
        schema_version: 2,
        error: "consent_required",
        message: "External access is off.",
        settings_path: "Settings > Agents > External access",
      }),
      exit: 1,
    });

    const failed = await errorOf("sona_meetings", {});

    expect(failed.code).toBe(ErrorCode.InvalidRequest);
    expect(failed.message).toContain("External access is off");
    expect(failed.message).toContain("Settings > Agents > External access");
    expect(failed.data).toEqual({
      code: "consent_required",
      settingsPath: "Settings > Agents > External access",
    });
  });

  test("a Sona usage refusal remains invalid parameters through tools/call", async () => {
    stub({
      stderr: JSON.stringify({
        schema_version: 2,
        error: "invalid_request",
        message: "--last is only read by --meetings.",
      }),
      exit: 2,
    });

    const failed = await errorOf("sona_search", { query: "Dana" });

    expect(failed.code).toBe(ErrorCode.InvalidParams);
    expect(failed.data).toEqual({
      code: "invalid_request",
      settingsPath: undefined,
    });
  });

  /* The other half of that branch. A caller told "internal error" retries or
   * gives up; a caller told "invalid params" fixes the id it passed. Sona
   * already distinguishes the two, so losing it here would be this server's
   * doing. The token leads the message because most clients render the
   * message and nothing else. */
  test("a corpus refusal is the caller's fault only when Sona says it is", async () => {
    stub({
      stderr: JSON.stringify({
        schema_version: 2,
        error: "not_found",
        message: "No loop m:loop:abc in this corpus.",
      }),
      exit: 1,
    });
    const missing = await errorOf("sona_loop_resolve", {
      loop_id: "m:loop:abc",
    });

    stub({
      stderr: JSON.stringify({
        schema_version: 2,
        error: "failed",
        message: "The corpus read failed.",
      }),
      exit: 1,
    });
    const broke = await errorOf("sona_search", { query: "notes" });

    expect(missing.code).toBe(ErrorCode.InvalidParams);
    expect(missing.message).toContain("sona not_found:");
    expect(missing.data).toEqual({
      code: "not_found",
      settingsPath: undefined,
    });
    expect(broke.code).toBe(ErrorCode.InternalError);
    expect(broke.message).toContain("sona failed:");
    expect(broke.data).toEqual({ code: "failed", settingsPath: undefined });
  });

  /* Arguments come from a model rather than from a validating client, so the
   * two shapes a tool cannot build a command line out of — a nested value, and
   * a missing required field — have to be refused here as bad parameters, and
   * refused before anything is spawned. The stub writes its argv file only
   * when it runs, so a missing file is proof that it did not. */
  test("an argument no argv can be built from is refused without running sona", async () => {
    const sona = stub({ stdout: "{}" });

    const nested = await errorOf("sona_search", { query: { text: "dana" } });
    const missing = await errorOf("sona_people", {});

    expect(nested.code).toBe(ErrorCode.InvalidParams);
    expect(nested.message).toContain("flat object");
    expect(missing.code).toBe(ErrorCode.InvalidParams);
    expect(missing.message).toContain("name is required");
    expect(() => sona.argv()).toThrow();
  });

  test("a tool this server does not have is method-not-found", async () => {
    const failed = await errorOf("sona_delete_everything", {});

    expect(failed.code).toBe(ErrorCode.MethodNotFound);
    expect(failed.message).toContain("sona_delete_everything");
  });
});
