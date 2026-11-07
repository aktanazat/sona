/* Tool schemas map directly to Sona's CLI flags.
 * Sona owns validation, corpus reads and consent. This server holds no corpus
 * state and provides no alternate write path. */

import { z } from "zod";

/** Which nouns `sona_search` will look through. */
export const SCOPES = [
  "all",
  "meetings",
  "dictations",
  "people",
  "loops",
  "notes",
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
  readonly mutates?: true;
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

const PROJECT_ID = {
  type: "string",
  description:
    "The exact saved project uuid returned by sona_projects. Project titles are not ids.",
} as const;


const PERSON_ID = {
  type: "string",
  description:
    "The exact person uuid returned by sona_people. Do not guess identity from a name or calendar address.",
} as const;

const SINCE_UTC_MS = {
  type: "integer",
  description:
    "Comparison boundary in UTC milliseconds since the Unix epoch. Changes require retained evidence on both sides; missing earlier evidence is not proof nothing changed.",
} as const;
export const TOOLS: readonly ToolDefinition[] = [
  {
    name: "sona_search",
    title: "Search Sona",
    description:
      "Search retained meetings, dictations, people, loops and notes. Results include matching text and a sona:// link. Read reason before treating an empty result as complete; semantic_unavailable means only literal matches were available. Notes are local data exposed through Sona's external-access grant, not remote panel context.",
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
            "Which records to search. 'all' includes notes; selecting one scope skips other sources.",
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
      "Read one meeting's retained transcript with speaker labels and offsets. text stays the effective corrected reading; raw_text preserves captured words, corrected_text is the separate human edit or null. Segment, transcript revision and edit revision identify the evidence. Removed lines remain excluded.",
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
    name: "sona_person_catch_up",
    title: "Catch up before talking",
    description:
      "Read a person's sourced catch-up, attributed words, linked notes, explicit assignments and evidence differences from Sona's shared person-memory snapshot. catch_up contains ids into the snapshot, not generated answers. Quotes, corrections and AI ledger interpretation stay distinct. Read limitations and cite source links; no identity or commitment is inferred from a matching name.",
    inputSchema: {
      type: "object",
      properties: {
        person_id: PERSON_ID,
        since_utc_ms: SINCE_UTC_MS,
      },
      required: ["person_id"],
      additionalProperties: false,
    },
    argv: (input) => [
      "--person-catch-up",
      text(input, "person_id", true),
      ...optional("--since-utc-ms", count(input, "since_utc_ms")),
    ],
  },
  {
    name: "sona_person_changes",
    title: "Read what changed",
    description:
      "Compare a person's retained source evidence since a UTC boundary. Each snapshot.changes entry includes earlier and later text, separate source links and interpretation labels. A text difference is not proof a decision changed or that two statements contradict. No comparable evidence means unknown, not no change. Uses the same snapshot as the native person page and catch-up.",
    inputSchema: {
      type: "object",
      properties: {
        person_id: PERSON_ID,
        since_utc_ms: SINCE_UTC_MS,
      },
      required: ["person_id", "since_utc_ms"],
      additionalProperties: false,
    },
    argv: (input) => {
      const since = count(input, "since_utc_ms");
      if (since === null) throw new SonaInputError("since_utc_ms is required.");
      return [
        "--person-changes",
        text(input, "person_id", true),
        "--since-utc-ms",
        since,
      ];
    },
  },
  {
    name: "sona_person_stats",
    title: "Read a person's numbers",
    description:
      "Read the numbers a person's page in Sona leads with: confirmed and suggested meeting counts, time recorded together, when you last met, their average talk share in permille, ledger outcomes (landed, open, dropped), open loops you owe and owed to you, confirmed meetings in each of the last 52 local weeks (oldest first, weeks start Monday, null before the corpus's first meeting) and the usual gap in days between meeting days. Suggested links count only as suggested. Unavailable numbers are null, not zero. No transcript text, titles or names. Requires external access; no writes.",
    inputSchema: {
      type: "object",
      properties: { person_id: PERSON_ID },
      required: ["person_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--person-stats", text(input, "person_id", true)],
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
    name: "sona_plans",
    title: "List plans heard in meetings",
    description:
      "List the plans to meet that were said aloud in recorded meetings, newest first: what, the time as said and the time Sona read it as (null when the words named none; all_day when they named a day only), the people and place as said, the exact quote, and status — heard, on_calendar, happened (a later recording overlapped it with one of its people) or no_record (its time passed with no such recording). Plans from a series kept on this Mac are left out. Requires external access; no writes, and nothing is added to any calendar.",
    inputSchema: {
      type: "object",
      properties: { limit: LIMIT },
      additionalProperties: false,
    },
    argv: (input) => ["--plans", ...optional("--limit", count(input, "limit"))],
  },
  {
    name: "sona_projects",
    title: "List Sona personal projects",
    description:
      "List saved local personal projects, most recently updated first, with their saved titles, states, revisions, task counts and next saved task or milestone due date. Returns at most 100 rows. Read has_more and complete before claiming the list is complete. Requires Sona's existing external-access grant; no project is changed.",
    inputSchema: {
      type: "object",
      properties: { limit: LIMIT },
      additionalProperties: false,
    },
    argv: (input) => ["--projects", ...optional("--limit", count(input, "limit"))],
  },
  {
    name: "sona_project",
    title: "Read a Sona personal project",
    description:
      "Read one saved personal project with its description, its pulse (done, open and overdue task counts, next due date, meetings in the last 7 days, and person ids owning open work), milestones, tasks, states, revisions and exact source references. Owner and source availability are resolved live by the native planner. Unavailable sources have available:false, no title and no link. Nullable due dates are saved values, not inferred deadlines. Owners are explicit personal-plan assignments, not team membership. Requires external access; no writes, notifications or provider calls.",
    inputSchema: {
      type: "object",
      properties: { project_id: PROJECT_ID },
      required: ["project_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--project", text(input, "project_id", true)],
  },
  {
    name: "sona_notes",
    title: "List Sona notes",
    description:
      "List retained notes, most recently updated first. Read one with sona_note for its body and source links. No note is changed.",
    inputSchema: {
      type: "object",
      properties: { limit: LIMIT },
      additionalProperties: false,
    },
    argv: (input) => ["--notes", ...optional("--limit", count(input, "limit"))],
  },
  {
    name: "sona_note",
    title: "Read a Sona note",
    description:
      "Read one note with linked source records, saved passage references, interpretation label, related notes and connection reasons. interpretation:true means the note is AI interpretation, not a direct transcript quote. Deleted or unavailable sources have available:false and disclose no source title, content or link.",
    inputSchema: {
      type: "object",
      properties: {
        note_id: {
          type: "string",
          description:
            "The note uuid from sona_notes, sona_search or a sona://note/<id> link.",
        },
      },
      required: ["note_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--note", text(input, "note_id", true)],
  },
  {
    name: "sona_note_context",
    title: "Read relevant note context",
    description:
      "Find matching retained notes with matched_text, current note bodies, resolved sources, passage references, interpretation labels and connection reasons. Uses the requested local note-search page limit; related-note bodies outside the page are not exported. interpretation:true is not a direct quote. Read has_more and reason. Unavailable sources expose no previous content. No provider is called and no note is changed.",
    inputSchema: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description: "Words remembered from a note. Every word must match.",
        },
        limit: LIMIT,
      },
      required: ["query"],
      additionalProperties: false,
    },
    argv: (input) => [
      "--note-context",
      text(input, "query", true),
      ...optional("--limit", count(input, "limit")),
    ],
  },
  {
    name: "sona_note_versions",
    title: "List saved note versions",
    description:
      "List a note's retained saved versions, newest first, with version ids, save times, kinds and sizes. Autosaves may merge within ten minutes; at most 50 versions are retained. Use these ids with sona_note_diff.",
    inputSchema: {
      type: "object",
      properties: {
        note_id: { type: "string", description: "The note uuid." },
      },
      required: ["note_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--note-versions", text(input, "note_id", true)],
  },
  {
    name: "sona_note_diff",
    title: "Compare saved note versions",
    description:
      "Read the exact line replacement between two retained versions without changing the note. from_source and to_source identify both saved versions with source links, times and kinds. Line text preserves original line endings. This is one replacement hunk, not a minimal edit script. Omit to_version to compare with the newest saved version.",
    inputSchema: {
      type: "object",
      properties: {
        note_id: { type: "string", description: "The note uuid." },
        from_version: {
          type: "integer",
          minimum: 1,
          description: "A saved version id from sona_note_versions.",
        },
        to_version: {
          type: "integer",
          minimum: 1,
          description:
            "The destination saved version id; defaults to newest saved version.",
        },
      },
      required: ["note_id", "from_version"],
      additionalProperties: false,
    },
    argv: (input) => {
      const from = count(input, "from_version");
      if (from === null) throw new SonaInputError("from_version is required.");
      return [
        "--note-diff",
        text(input, "note_id", true),
        "--from-version",
        from,
        ...optional("--to-version", count(input, "to_version")),
      ];
    },
  },
  {
    name: "sona_stats",
    title: "Read Sona corpus statistics",
    description:
      "Read content-free statistics for all retained meetings, dictations and notes, with measured time, population, counting methods and up to 25 sample source links per dataset. Sample links do not enumerate every counted record. Each dataset or source sample can be unavailable with an error; complete:false means partial coverage. Never treat unavailable as zero. Meeting duration counts verified audio intervals once even when capture sources overlap.",
    inputSchema: {
      type: "object",
      properties: {},
      additionalProperties: false,
    },
    argv: () => ["--stats"],
  },
  {
    name: "sona_events",
    title: "Read Sona events",
    description:
      "Read receipts and workflow results, newest first. Pass next_cursor as after to continue. This read does not change the corpus.",
    inputSchema: {
      type: "object",
      properties: {
        after: {
          type: "string",
          description: "An event next_cursor from the previous page.",
        },
        limit: LIMIT,
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "--events",
      ...optional("--after", text(input, "after")),
      ...optional("--limit", count(input, "limit")),
    ],
  },
  {
    name: "sona_loop_resolve",
    title: "Resolve a Sona loop",
    description:
      "Mark one loop or commitment done, and get back the operation receipt. It reads the loop revision before writing, so it requires both Settings > Agents > External access and External mutations. If either is off, Sona returns consent_required and names the missing setting. The receipt's result is 'committed' when the write landed and 'rejected' when the meeting moved underneath it.",
    mutates: true,
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
