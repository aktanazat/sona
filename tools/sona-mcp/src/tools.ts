/* The eight tools, as data.
 *
 * Each one is a name, a schema an agent reads, and the argv it becomes. The
 * mapping is 1:1 with `sona`'s headless flags and deliberately does nothing
 * else: no defaults invented here, no ranges re-checked here. Sona owns what a
 * scope is, what a limit may be and what a meeting id looks like, and it
 * answers a bad one with a typed JSON refusal this server passes straight
 * back. A second validator here would be a second opinion, and the two would
 * drift.
 *
 * Seven of them only read. `sona_loop_resolve` reads a loop revision before
 * it writes, so Sona requires both external-access rows and returns whichever
 * `consent_required` refusal names the missing row.
 *
 * The only checking below is what building an argv actually requires: a
 * required string has to be there, or there is no command to run. */

import { z } from "zod";

/** Which nouns `sona_search` will look through. */
export const SCOPES = [
  "all",
  "meetings",
  "dictations",
  "people",
  "loops",
] as const;

/** Which loop rows `sona_action_items` keeps. */
export const STATUSES = ["open", "done"] as const;

/** Whose side of a loop `sona_action_items` keeps. */
export const SIDES = ["mine", "waiting"] as const;

/* The one shape an argv can be built out of: a flat object of JSON scalars,
 * keyed by field name. Nothing here re-checks a value it can turn into an
 * argument — Sona owns what a scope is and what a limit may be — so the schema
 * stops at "flat", which is the only property this file actually relies on. */
export const TOOL_INPUT = z.record(
  z.string(),
  z.union([z.string(), z.number(), z.boolean(), z.null()]),
);

export type ToolInput = z.infer<typeof TOOL_INPUT>;

/** One JSON-schema property as this server publishes it: a flat string or
 * integer field, optionally enumerated and bounded. Typed so readers get the
 * enum without re-parsing the schema. */
export interface ToolProperty {
  readonly type: "string" | "integer";
  readonly description: string;
  readonly enum?: readonly string[];
  readonly minimum?: number;
  readonly maximum?: number;
}

export interface ToolDefinition {
  readonly name: string;
  readonly title: string;
  readonly description: string;
  readonly inputSchema: {
    readonly type: "object";
    readonly properties: Record<string, ToolProperty>;
    readonly required?: readonly string[];
    readonly additionalProperties: false;
  };
  /** The `sona` command line this call becomes. */
  argv(input: ToolInput): string[];
}

/** An argument this server could not turn into a command line. */
export class SonaInputError extends Error {}

function text(input: ToolInput, key: string, required: true): string;
function text(input: ToolInput, key: string, required?: false): string | null;
function text(input: ToolInput, key: string, required = false): string | null {
  const value = input[key];
  if (value === undefined || value === null || value === "") {
    if (required) throw new SonaInputError(`${key} is required.`);
    return null;
  }
  const parsed = z.string().safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError(`${key} must be a string.`);
  }
  return parsed.data;
}

/** A count, as the flag spells it. Sona bounds it; this only stringifies. */
function count(input: ToolInput, key: string): string | null {
  const value = input[key];
  if (value === undefined || value === null) return null;
  const parsed = z.number().int().safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError(`${key} must be a whole number.`);
  }
  return String(parsed.data);
}

function optional(flag: string, value: string | null): string[] {
  return value === null ? [] : [flag, value];
}

const LIMIT = {
  type: "integer",
  minimum: 1,
  maximum: 100,
  description: "How many rows to return. Sona defaults to 25 and caps at 100.",
} as const;

const MEETING_ID = {
  type: "string",
  description:
    "The meeting's uuid, as it appears in a sona://meeting/<id> link or in the id field of a sona_search or sona_meetings row.",
} as const;

export const TOOLS: readonly ToolDefinition[] = [
  {
    name: "sona_search",
    title: "Search Sona",
    description:
      "Search everything Sona keeps — meetings, dictations, people and open loops — and get back the rows that matched, each with the text that matched it and a sona:// address. Newest first. `reason` names whatever shaped the page other than the corpus: `no_searchable_tokens` when the query held no word to match, `semantic_unavailable` when the recall model is not on this machine and only literal word matching ran, `no_rows` when every source was read and nothing matched. Null means the rows are all there are.",
    inputSchema: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description:
            "What to look for. Words are ANDed: every one has to appear.",
        },
        scope: {
          type: "string",
          enum: [...SCOPES],
          description:
            "Which nouns to search. 'all' (the default) searches meetings, dictations, people and loops; the others narrow to exactly one of those. Narrowing skips the other sources rather than filtering the page afterwards.",
        },
        limit: LIMIT,
      },
      required: ["query"],
      additionalProperties: false,
    },
    argv: (input) => [
      "--query",
      text(input, "query", true),
      ...optional("--scope", text(input, "scope")),
      ...optional("--limit", count(input, "limit")),
    ],
  },
  {
    name: "sona_meetings",
    title: "List Sona meetings",
    description:
      "List retained meetings, newest first, with each one's status, recorded length, speakers and one-line headline.",
    inputSchema: {
      type: "object",
      properties: {
        last: {
          type: "integer",
          minimum: 1,
          maximum: 100,
          description:
            "How many of the most recent meetings to return. Cannot be combined with from/to.",
        },
        from: {
          type: "string",
          description:
            "Earliest local day to include, as YYYY-MM-DD. Requires 'to'.",
        },
        to: {
          type: "string",
          description:
            "Latest local day to include, as YYYY-MM-DD. The whole day is included. Requires 'from'.",
        },
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "--meetings",
      ...optional("--last", count(input, "last")),
      ...optional("--from", text(input, "from")),
      ...optional("--to", text(input, "to")),
    ],
  },
  {
    name: "sona_meeting",
    title: "Read a Sona meeting",
    description:
      "Read one meeting: its generated summary, the ledger headline, the notes typed during it, and every loop and commitment it left, each with who owes it and whether it is done.",
    inputSchema: {
      type: "object",
      properties: { meeting_id: MEETING_ID },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--meeting", text(input, "meeting_id", true)],
  },
  {
    name: "sona_transcript",
    title: "Read a Sona transcript",
    description:
      "Read one meeting's transcript, speaker-labeled, with each line's offset in milliseconds. Human edits are applied and removed lines are gone.",
    inputSchema: {
      type: "object",
      properties: { meeting_id: MEETING_ID },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--transcript", text(input, "meeting_id", true)],
  },
  {
    name: "sona_action_items",
    title: "List Sona action items",
    description:
      "List loops and commitments across every meeting. Pass next_cursor from an earlier result as after, with the same filters, to continue after that loop. `reason` says why a page is empty: `awaiting_continuity` when a meeting's continuity pass has not run and its rows are not reportable yet, `filtered_out` when the status or side filter excluded every row, `no_rows` when the corpus holds none.",
    inputSchema: {
      type: "object",
      properties: {
        status: {
          type: "string",
          enum: [...STATUSES],
          description:
            "Keep only rows in this state. Omit to get every state, including dropped and carried-forward rows.",
        },
        side: {
          type: "string",
          enum: [...SIDES],
          description:
            "Keep only rows on one side of the conversation: 'mine' is what the user owes, 'waiting' is what somebody else owes them.",
        },
        after: {
          type: "string",
          description:
            "The next_cursor from an earlier result. Keep the same status and side filters when resuming.",
        },
        limit: LIMIT,
      },
      additionalProperties: false,
    },
    argv: (input) => {
      const side = text(input, "side");
      return [
        "--loops",
        ...optional("--status", text(input, "status")),
        ...(side === null ? [] : [side === "mine" ? "--mine" : "--waiting"]),
        ...optional("--after", text(input, "after")),
        ...optional("--limit", count(input, "limit")),
      ];
    },
  },
  {
    name: "sona_people",
    title: "Find people in Sona",
    description:
      "Look somebody up by name, alias or calendar address, and get back their profile: how many meetings they were in, when the last one was, and what it left. `reason` says why a lookup found nobody: `people_index_empty` when diarization has not named anybody yet, so no name can match, or `no_rows` when people are indexed and this name is not one of them.",
    inputSchema: {
      type: "object",
      properties: {
        name: {
          type: "string",
          description:
            "A name, alias or calendar address. Words are ANDed: every one has to appear in the names the person answers to.",
        },
        limit: LIMIT,
      },
      required: ["name"],
      additionalProperties: false,
    },
    argv: (input) => [
      "--people",
      text(input, "name", true),
      ...optional("--limit", count(input, "limit")),
    ],
  },
  {
    name: "sona_upcoming",
    title: "List upcoming meetings",
    description:
      "List today and the next seven days of calendar: each event's title, times, attendees, join link, and — for a recurring one — its series key and whether it records itself. Returns calendar_access_subject set to responsible_process and calendar_access. On macOS the latter is the TCC result for the headless caller's responsible process, often the terminal, not a claim about the installed GUI. An empty list under anything but 'authorized' means this request cannot read the calendar, not that the GUI lacks a grant.",
    inputSchema: {
      type: "object",
      properties: { limit: LIMIT },
      additionalProperties: false,
    },
    argv: (input) => [
      "--upcoming",
      ...optional("--limit", count(input, "limit")),
    ],
  },
  {
    name: "sona_loop_resolve",
    title: "Resolve a Sona loop",
    description:
      "Mark one loop or commitment done, and get back the operation receipt. It reads the loop revision before writing, so it requires both Settings > Agents > External access and External mutations. If either is off, Sona returns consent_required and names the missing setting. The receipt's result is 'committed' when the write landed and 'rejected' when the meeting moved underneath it.",
    inputSchema: {
      type: "object",
      properties: {
        loop_id: {
          type: "string",
          description:
            "The loop's own address, as it appears in a sona://loop/<id> link or in the id field of a sona_action_items row.",
        },
      },
      required: ["loop_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--loop-resolve", text(input, "loop_id", true)],
  },
];
