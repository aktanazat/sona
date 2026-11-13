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
  "Sona holds the user's meetings, notes, tasks, projects and people. Start with sona_work_today for what matters now. For tasks: sona_work_counts or sona_work_list (ready:true for what to start next; filter takes {field, op, values} conditions such as label includes_all or due before today), sona_work_get for one task with its meeting quote, parent, sub-tasks, comments, attachments and history, sona_work_claim before starting work, sona_work_done with evidence after. Work Sona pulled out of meetings waits in triage: sona_work_triage_suggestions lists what Sona suggests for each item (owner, due day, project, duplicate) with the quote behind it, sona_work_suggestion_accept or sona_work_suggestion_dismiss decides one suggestion, and sona_work_triage accepts, declines or marks items duplicate. When you work a task over time, open a session with sona_session_start, post progress with sona_session_activity and sona_session_plan, and keep it alive with sona_session_heartbeat; the user sees it on the task page. End the session with a sona_session_activity of type response (the result) or error (why you stopped); sona_session_stop is the user asking you to stop, not how you end. sona_task_save changes several fields of one task in one call, and with no task makes a new one from a title step; its steps can also delegate the task, add or remove blocks, blocked_by and related links, mark it a duplicate, attach links and link a meeting as its source. To stay in sync without re-reading everything, poll sona_work_changes with the cursor it returned last time. While you work a task you claimed, say what you are doing on it with sona_comment_save: a comment when you start, when you find something the user should know, when you are blocked, and what is left when you stop; read sona_comment_list first and reply in a thread with reply_to instead of starting a new one. Attach the artifacts you produce for a task, such as a report, a screenshot or a pull request link, with sona_task_attach (a file by absolute path, or a url); never attach secrets, keys, tokens or credentials. Break a task into pieces with sona_subtask_create, and move a task under another with sona_work_update parent (null lifts it back out). Handles like S-12 work wherever a task is asked for. Wherever a person is asked for, a name or any name they go by works; look people up with sona_person_list. A task owner also takes me (the user) or nobody, and a project lead or member takes me. Projects: sona_project_list, sona_project_detail for progress, the predicted finish, the latest update and the linked notes and meetings, sona_project_history for what changed, sona_milestone_list and sona_milestone_get, sona_project_save and sona_milestone_save, which make the thing when nothing has that name and change it when something does, and sona_milestone_reorder. A project's status_name comes from sona_project_status_list, and sona_project_save patch edits its description or summary at an anchor. Post how a project is going with sona_project_update_save (read sona_project_update_list first; draft:true with dry_run:true shows what Sona would write). Meetings with who was in them: sona_meeting_list, then sona_meeting_get. The user's notes: sona_notes lists them, sona_note reads one, sona_note_context finds the notes that bear on a question, and sona_note_versions with sona_note_diff show how one changed. " +
  "Initiatives are goals one level above projects: read them with sona_initiative_list and sona_initiative_get, make or change one with sona_initiative_new and sona_initiative_update, and move projects in and out with sona_initiative_add_project and sona_initiative_remove_project. Task templates: sona_work_templates lists them, sona_work_template_save makes or changes one with its fields, labels, repeat and sub-tasks, and sona_work_template_use makes a task from one. Project templates: sona_project_template_list lists them, sona_project_template_save saves a project as one, and sona_project_new with template starts a project from one. When one project waits on another, sona_project_dependency_list lists those links and sona_project_relate and sona_project_unrelate add or remove one. sona_work_insights reads how long work takes: created to done, and time spent in triage. sona_work_digest, with SONA_MCP_TOOLS=all, reads the weekly project digest: each project's status, progress, latest update, the tasks done that week and its risks. sona_work_remind sets or clears when Sona reminds the user of a task, and sona_work_reminders lists the reminders, soonest first. Favorites, with SONA_MCP_TOOLS=all: sona_work_favorites reads the user's sidebar favorites, sona_work_favorite and sona_work_unfavorite add or remove one, and the favorite folder tools arrange them. " +
  "In project, milestone, person and cycle saves a null field clears it. Writes need the user's External mutations setting; preview a task change with dry_run:true, and pass the same operation_id when retrying so nothing is done twice. Unless the user lets agents' changes apply, a change to a task waits under proposed for the user to accept in Sona, new tasks and sub-tasks land in Triage, and project, update, cycle and people writes are refused; comments and attachments are added either way. To share a meeting page, sona_share_preview shows it and sona_share_prepare leaves it for the user to publish in Sona; no tool publishes. Set SONA_MCP_TOOLS=all for cycles, labels, views, links, archiving, the project bin, comment, attachment, update, milestone and project status name deletes, turning a milestone into a project, person merges, naming meeting speakers, meetings, settings, automations, writing notes (sona_scratchpad_*), handing a task to another agent (sona_work_delegate), the user's inbox of task notifications (sona_work_inbox, sona_work_inbox_mark, sona_work_subscribe), watched topics and the Markdown export (sona_work_export).";

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
        idempotentHint: tool.mutates === undefined || tool.idempotent === true,
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
