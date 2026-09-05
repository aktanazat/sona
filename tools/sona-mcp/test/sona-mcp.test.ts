/* What this server actually does: turn a tool call into a `sona` command line,
 * and hand back what `sona` said.
 *
 * The stub below is most of the test rig. It records the argv it was given and
 * replies with whatever the test told it to, which is enough to pin the mapping
 * and the passthrough without an installed app and without a corpus. The last
 * describe adds the piece a stub cannot reach: `index.ts` on a real MCP
 * transport, so the server's construction — its imports included — is exercised
 * by the suite rather than first by an agent's first `tools/call`.
 */

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
import {
  DEFAULT_SONA_BIN,
  runSona,
  SonaCliError,
  sonaBinary,
} from "../src/cli.ts";
import {
  SCOPES,
  SIDES,
  STATUSES,
  SonaInputError,
  type ToolDefinition,
  type ToolInput,
  TOOLS,
} from "../src/tools.ts";
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

/** The values a tool's schema publishes for one property. */
function publishedEnum(toolName: string, property: string): readonly string[] {
  const published = tool(toolName).inputSchema.properties[property]?.enum;
  if (published === undefined) {
    throw new Error(`${toolName}.${property} publishes no enum`);
  }
  return published;
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

describe("tool to argv", () => {
  test("every tool maps onto one headless sona verb", () => {
    expect(TOOLS.map((definition) => definition.name)).toEqual([
      "sona_search",
      "sona_meetings",
      "sona_meeting",
      "sona_transcript",
      "sona_action_items",
      "sona_people",
      "sona_upcoming",
      "sona_loop_resolve",
    ]);
    expect(
      TOOLS.map((definition) => definition.argv(minimalInput(definition))[0]),
    ).toEqual([
      "--query",
      "--meetings",
      "--meeting",
      "--transcript",
      "--loops",
      "--people",
      "--upcoming",
      "--loop-resolve",
    ]);
  });

  test("upcoming takes only a row count", () => {
    expect(tool("sona_upcoming").argv({})).toEqual(["--upcoming"]);
    expect(tool("sona_upcoming").argv({ limit: 3 })).toEqual([
      "--upcoming",
      "--limit",
      "3",
    ]);
  });

  test("upcoming names the TCC subject", () => {
    expect(tool("sona_upcoming").description).toContain("responsible_process");
  });

  /* An empty array is two answers, and an agent that cannot tell them apart
   * either retries a question that was fully answered or stops on one that was
   * not. Each read that can hide a degraded source publishes the value that
   * says which happened, so an agent reading the schema knows to look. */
  test("every read that can come back short names why", () => {
    expect(tool("sona_search").description).toContain("semantic_unavailable");
    expect(tool("sona_action_items").description).toContain(
      "awaiting_continuity",
    );
    expect(tool("sona_people").description).toContain("people_index_empty");
  });
  /* The one tool that writes. Its loop id goes through verbatim, and its
   * schema says so is required, so a call without one never spawns anything. */
  test("resolving a loop carries the loop id and nothing else", () => {
    expect(
      tool("sona_loop_resolve").argv({ loop_id: "abc:loop:0123456789abcdef" }),
    ).toEqual(["--loop-resolve", "abc:loop:0123456789abcdef"]);
    expect(() => tool("sona_loop_resolve").argv({})).toThrow(SonaInputError);
  });

  test("search carries its scope and limit", () => {
    expect(
      tool("sona_search").argv({ query: "dana", scope: "people", limit: 5 }),
    ).toEqual(["--query", "dana", "--scope", "people", "--limit", "5"]);
    expect(tool("sona_search").argv({ query: "dana" })).toEqual([
      "--query",
      "dana",
    ]);
  });

  test("a meetings window is either a count or a pair of dates", () => {
    expect(tool("sona_meetings").argv({ last: 3 })).toEqual([
      "--meetings",
      "--last",
      "3",
    ]);
    expect(
      tool("sona_meetings").argv({ from: "2026-06-01", to: "2026-06-30" }),
    ).toEqual(["--meetings", "--from", "2026-06-01", "--to", "2026-06-30"]);
  });

  test("action items narrow by state and side, then resume after a loop", () => {
    expect(tool("sona_action_items").argv({ status: "open" })).toEqual([
      "--loops",
      "--status",
      "open",
    ]);
    expect(tool("sona_action_items").argv({ side: "mine" })).toEqual([
      "--loops",
      "--mine",
    ]);
    expect(
      tool("sona_action_items").argv({ side: "waiting", limit: 10 }),
    ).toEqual(["--loops", "--waiting", "--limit", "10"]);
    expect(
      tool("sona_action_items").argv({
        status: "open",
        side: "waiting",
        after: "loop-123",
        limit: 10,
      }),
    ).toEqual([
      "--loops",
      "--status",
      "open",
      "--waiting",
      "--after",
      "loop-123",
      "--limit",
      "10",
    ]);
  });

  test("a meeting id and a name go through verbatim", () => {
    expect(tool("sona_meeting").argv({ meeting_id: "abc" })).toEqual([
      "--meeting",
      "abc",
    ]);
    expect(tool("sona_transcript").argv({ meeting_id: "abc" })).toEqual([
      "--transcript",
      "abc",
    ]);
    expect(tool("sona_people").argv({ name: "Dana Reyes" })).toEqual([
      "--people",
      "Dana Reyes",
    ]);
  });

  /* Sona owns what a scope is. This server only has to publish the same list,
   * so a client picking from the schema cannot compose a command Sona refuses
   * for a reason the schema could have prevented. */
  test("the schemas publish the enums sona accepts", () => {
    expect(publishedEnum("sona_search", "scope")).toEqual([...SCOPES]);
    expect(publishedEnum("sona_action_items", "status")).toEqual([...STATUSES]);
    expect(publishedEnum("sona_action_items", "side")).toEqual([...SIDES]);
    expect(SCOPES).toEqual([
      "all",
      "meetings",
      "dictations",
      "people",
      "loops",
    ]);
  });

  test("an argument that cannot become a command line is refused here", () => {
    expect(() => tool("sona_search").argv({})).toThrow(SonaInputError);
    expect(() => tool("sona_people").argv({ name: 42 })).toThrow(
      SonaInputError,
    );
    expect(() => tool("sona_search").argv({ query: "x", limit: 1.5 })).toThrow(
      SonaInputError,
    );
  });
});

describe("running sona", () => {
  test("the resolved binary is the install path unless SONA_BIN says otherwise", () => {
    expect(sonaBinary()).toBe(DEFAULT_SONA_BIN);
    process.env.SONA_BIN = "/somewhere/else/sona";
    expect(sonaBinary()).toBe("/somewhere/else/sona");
  });

  test("a tool call reaches the binary as the argv the tool built", async () => {
    const sona = stub({ stdout: '{"schema_version":2,"entries":[]}' });

    const answer = await runSona(
      tool("sona_search").argv({ query: "tier comparison", scope: "meetings" }),
    );

    expect(sona.argv()).toEqual([
      "--query",
      "tier comparison",
      "--scope",
      "meetings",
    ]);
    expect(answer).toEqual({ schema_version: 2, entries: [] });
  });

  /* The refusal this whole feature exists to gate on. It has to survive the
   * trip unchanged: the code an agent branches on, and the settings row it
   * tells its human to click. */
  test("a consent refusal comes back with its code and its settings path", async () => {
    stub({
      stderr: JSON.stringify({
        schema_version: 2,
        error: "consent_required",
        message:
          "External access is off. Turn on Settings > Agents > External access in Sona to allow read-only corpus queries.",
        settings_path: "Settings > Agents > External access",
      }),
      exit: 1,
    });

    const refused = await refusalOf(["--loops"]);

    expect(refused.code).toBe("consent_required");
    expect(refused.settingsPath).toBe("Settings > Agents > External access");
    expect(refused.exitCode).toBe(1);
    expect(refused.message).toContain("External access is off");
  });

  /* The mutation row is a second grant, and its refusal names its own row. A
   * caller that read the corpus a moment ago still has to be told which switch
   * this one needs. */
  test("a mutation refusal names the mutations row, not the read one", async () => {
    stub({
      stderr: JSON.stringify({
        schema_version: 2,
        error: "consent_required",
        message:
          "External mutations are off. Turn on Settings > Agents > External mutations in Sona to allow changes to the corpus.",
        settings_path: "Settings > Agents > External mutations",
      }),
      exit: 1,
    });

    const refused = await refusalOf(["--loop-resolve", "abc:loop:0123"]);

    expect(refused.code).toBe("consent_required");
    expect(refused.settingsPath).toBe("Settings > Agents > External mutations");
  });

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

/** The least each tool needs to build a command line at all: one placeholder
 *  for every field its own schema marks required. Read off the schema rather
 *  than restated per tool, so a new required field cannot be forgotten here. */
const minimalInput = (tool: ToolDefinition): ToolInput =>
  Object.fromEntries(
    (tool.inputSchema.required ?? []).map((field) => [field, "abc"]),
  );

/** A client talking to a freshly constructed server over linked in-memory
 * transports. Constructing it is the point: `createServer` is where the
 * `tools.ts` imports are resolved and the two request handlers are registered,
 * and nothing else in this suite touches either. */
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
  test("publishes every tool it can run", async () => {
    const client = await connected();

    const listed = await client.listTools();

    expect(listed.tools.map((published) => published.name)).toEqual(
      TOOLS.map((definition) => definition.name),
    );
    /* The schema an agent reads is the one in `tools.ts`, verbatim: through
     * JSON because that is how it reaches the agent. */
    expect(listed.tools.map((published) => published.inputSchema)).toEqual(
      JSON.parse(
        JSON.stringify(TOOLS.map((definition) => definition.inputSchema)),
      ),
    );
    await client.close();
  });

  test("every published tool reaches isolated sona with its documented argv", async () => {
    const cases: ReadonlyArray<{
      name: string;
      args: RawCallArguments;
      argv: string[];
    }> = [
      {
        name: "sona_search",
        args: { query: "tier" },
        argv: ["--query", "tier"],
      },
      {
        name: "sona_meetings",
        args: { last: 3 },
        argv: ["--meetings", "--last", "3"],
      },
      {
        name: "sona_meeting",
        args: { meeting_id: "meeting-1" },
        argv: ["--meeting", "meeting-1"],
      },
      {
        name: "sona_transcript",
        args: { meeting_id: "meeting-1" },
        argv: ["--transcript", "meeting-1"],
      },
      {
        name: "sona_action_items",
        args: { status: "open", side: "mine", after: "loop-1", limit: 2 },
        argv: [
          "--loops",
          "--status",
          "open",
          "--mine",
          "--after",
          "loop-1",
          "--limit",
          "2",
        ],
      },
      {
        name: "sona_people",
        args: { name: "Dana" },
        argv: ["--people", "Dana"],
      },
      {
        name: "sona_upcoming",
        args: { limit: 2 },
        argv: ["--upcoming", "--limit", "2"],
      },
      {
        name: "sona_loop_resolve",
        args: { loop_id: "loop-1" },
        argv: ["--loop-resolve", "loop-1"],
      },
    ];

    for (const { name, args, argv } of cases) {
      const payload = { schema_version: 2, tool: name };
      const sona = stub({ stdout: JSON.stringify(payload) });
      const client = await connected();
      try {
        const answered = await client.callTool({ name, arguments: args });

        expect(sona.argv()).toEqual(argv);
        expect(answered.content).toEqual([
          { type: "text", text: JSON.stringify(payload, null, 2) },
        ]);
      } finally {
        await client.close();
      }
    }
  });

  test("a committed loop receipt reaches an MCP caller unchanged", async () => {
    const loopId = "1e1a5f0e-0000-4000-8000-000000000001:loop:0123456789abcdef";
    const payload = {
      schema_version: 2,
      receipt: {
        schema_version: 2,
        operation_id: "5c02eb2b-811d-53f1-8d9a-36cfd7e89b85",
        session_id: "1e1a5f0e-0000-4000-8000-000000000001",
        actor: "user",
        command: "loop_resolve",
        expected_revision: 0,
        from_phase: "review_ready",
        to_phase: "review_ready",
        requested_at_utc_ms: 1_700_000_000_000,
        committed_at_utc_ms: 1_700_000_000_001,
        result: "committed",
        reason_codes: [],
        new_revision: 1,
        effect_ids: [loopId],
      },
    };
    const sona = stub({ stdout: JSON.stringify(payload) });
    const client = await connected();
    try {
      const answered = await client.callTool({
        name: "sona_loop_resolve",
        arguments: { loop_id: loopId },
      });

      expect(sona.argv()).toEqual(["--loop-resolve", loopId]);
      expect(answered.content).toEqual([
        { type: "text", text: JSON.stringify(payload, null, 2) },
      ]);
    } finally {
      await client.close();
    }
  });

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
