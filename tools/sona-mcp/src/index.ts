#!/usr/bin/env bun
/* The stdio MCP server over Sona's headless query surface.
 *
 * Transport and nothing else: tool definitions are data in `tools.ts`, running
 * the binary is `cli.ts`, and this file is what turns a `tools/call` into an
 * argv and a refusal into an MCP error. Everything an agent reads here came
 * out of Sona a moment ago; this server holds no state between calls.
 */

import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ErrorCode,
  ListToolsRequestSchema,
  McpError,
} from "@modelcontextprotocol/sdk/types.js";
import { runSona, SonaCliError } from "./cli.ts";
import { publishedTools, SonaInputError, TOOL_INPUT } from "./tools.ts";

/* Read by clients that show a server's instructions to their model: the
 * order an agent works a task in, which no single tool description holds. */
const INSTRUCTIONS =
  "Sona holds the user's meetings, notes and tasks. For tasks: sona_work_counts or sona_work_list (ready:true for what to start next), sona_work_get for one task with its meeting quote and history, sona_work_claim before starting work, sona_work_done with evidence after. Handles like S-12 work wherever a task is asked for. Writes need the user's External mutations setting; preview a change with dry_run:true. Unless the user lets agents' changes apply, a change to a task waits under proposed for the user to accept in Sona. Set SONA_MCP_TOOLS=all for labels, views, links and archiving.";

/* JSON-RPC's vocabulary over Sona's. Only three of Sona's refusals are the
 * caller's to fix: two name an argument that matches nothing in the corpus,
 * and one names a switch a human has to flip. Everything else — `unavailable`,
 * `failed`, `timed_out`, `not_installed` — happened on this side of the call,
 * and the default is where they land: an agent reads InternalError as "stop
 * and report", not "try different arguments". A switch rather than a lookup
 * table because Sona's code arrives as a string and the fall-through is the
 * half that carries the meaning. */
function refusalCode(refusal: string): ErrorCode {
  switch (refusal) {
    case "consent_required":
      return ErrorCode.InvalidRequest;
    case "invalid_request":
    case "not_found":
      return ErrorCode.InvalidParams;
    default:
      return ErrorCode.InternalError;
  }
}

/** The server, wired to the published CLI tools and to nothing else.
 *
 * Construction is separated from `connect` so a test can hold this server on
 * an in-memory transport. That is not ceremony: the ESM link between this file
 * and `tools.ts` is the one thing no other check covers — `bun test`
 * transpiles without type-checking and the root lint never reaches this
 * directory — so a test that never constructs the server cannot tell a missing
 * export from a working one. `selection` is `SONA_MCP_TOOLS`; an unknown tool
 * name in it throws here, so a typo stops the server at start. */
export function createServer(selection = process.env.SONA_MCP_TOOLS): Server {
  const tools = publishedTools(selection);
  const server = new Server(
    { name: "sona-mcp", version: "0.1.0" },
    { capabilities: { tools: {} }, instructions: INSTRUCTIONS },
  );

  server.setRequestHandler(ListToolsRequestSchema, () => ({
    tools: tools.map((tool) => ({
      name: tool.name,
      title: tool.title,
      description: tool.description,
      inputSchema: tool.inputSchema,
      annotations: { readOnlyHint: tool.mutates !== true },
    })),
  }));

  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const tool = tools.find(
      (candidate) => candidate.name === request.params.name,
    );
    if (tool === undefined) {
      throw new McpError(
        ErrorCode.MethodNotFound,
        `sona-mcp has no tool named ${request.params.name}.`,
      );
    }
    /* The transport is where a call first holds its arguments, so the schema
     * runs right here: a nested object reaches the agent as the bad parameter
     * it is rather than as a server fault. */
    const input = TOOL_INPUT.safeParse(request.params.arguments ?? {});
    if (!input.success) {
      throw new McpError(
        ErrorCode.InvalidParams,
        "Arguments must be a flat object of strings, numbers, booleans and nulls.",
      );
    }
    try {
      const argv = tool.argv(input.data, server.getClientVersion()?.name);
      const answer = await runSona(argv);
      /* One text block holding the JSON Sona printed. No `structuredContent`:
       * this server declares no output schema, so a second copy of the same
       * bytes in a second field would be payload rather than information. */
      return {
        content: [{ type: "text", text: JSON.stringify(answer, null, 2) }],
      };
    } catch (error) {
      if (error instanceof SonaInputError) {
        throw new McpError(ErrorCode.InvalidParams, error.message);
      }
      if (error instanceof SonaCliError) {
        /* Sona's refusal, passed through rather than reworded, with its own
         * word for what went wrong in front of it. `data.code` carries the
         * same word, but a coding agent reads the rendered message and most
         * clients render nothing else, so a cause left only in `data` is a
         * cause the caller never sees. */
        throw new McpError(
          refusalCode(error.code),
          error.settingsPath === undefined
            ? `sona ${error.code}: ${error.message}`
            : `sona ${error.code}: ${error.message} (${error.settingsPath})`,
          { code: error.code, settingsPath: error.settingsPath },
        );
      }
      throw error;
    }
  });

  return server;
}

/* Run as the binary: take stdio. Imported: do nothing, so a test can hold a
 * server of its own without racing this one for the same file descriptors. */
if (import.meta.main) {
  await createServer().connect(new StdioServerTransport());
}
