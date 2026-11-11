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
  "Sona holds the user's meetings, notes, tasks, projects and people. Start with sona_work_today for what matters now. For tasks: sona_work_counts or sona_work_list (ready:true for what to start next; filter takes {field, op, values} conditions such as label includes_all or due before today), sona_work_get for one task with its meeting quote, parent, sub-tasks, comments, attachments and history, sona_work_claim before starting work, sona_work_done with evidence after. When you work a task over time, open a session with sona_session_start, post progress with sona_session_activity and sona_session_plan, keep it alive with sona_session_heartbeat and close it with sona_session_stop; the user sees it on the task page. sona_task_save changes several fields of one task in one call. To stay in sync without re-reading everything, poll sona_work_changes with the cursor it returned last time. While you work a task you claimed, say what you are doing on it with sona_comment_save: a comment when you start, when you find something the user should know, when you are blocked, and what is left when you stop; read sona_comment_list first and reply in a thread with reply_to instead of starting a new one. Attach the artifacts you produce for a task, such as a report, a screenshot or a pull request link, with sona_task_attach (a file by absolute path, or a url); never attach secrets, keys, tokens or credentials. Break a task into pieces with sona_subtask_create, and move a task under another with sona_work_update parent (null lifts it back out). Handles like S-12 work wherever a task is asked for. Wherever a person is asked for, a name or any name they go by works; look people up with sona_person_list. A task owner also takes me (the user) or nobody, and a project lead or member takes me. Projects: sona_project_list, sona_project_detail for progress, the predicted finish and the latest update, sona_milestone_list, sona_project_save and sona_milestone_save, which make the thing when nothing has that name and change it when something does, and sona_milestone_reorder. Post how a project is going with sona_project_update_save (read sona_project_update_list first; draft:true with dry_run:true shows what Sona would write). Meetings with who was in them: sona_meeting_list, then sona_meeting_get. In project, milestone, person and cycle saves a null field clears it. Writes need the user's External mutations setting; preview a change with dry_run:true, and pass the same operation_id when retrying so nothing is done twice. Unless the user lets agents' changes apply, a change to a task waits under proposed for the user to accept in Sona, new tasks and sub-tasks land in Triage, and project, update, cycle and people writes are refused; comments and attachments are added either way. Set SONA_MCP_TOOLS=all for cycles, labels, views, links, archiving, the project bin, comment, attachment, update and milestone deletes, person merges, naming meeting speakers, meetings, settings and automations.";

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
      annotations: {
        readOnlyHint: tool.mutates === undefined,
        destructiveHint: tool.mutates === "destroys",
      },
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
        "Arguments must be a flat object of strings, numbers, booleans, nulls and lists of strings, or a filter list of {field, op, values} conditions.",
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
