/* Tool schemas map directly to Sona's CLI flags and `sona work` verbs.
 * Sona owns validation, corpus reads, tracker writes and consent. This server
 * holds no corpus state and provides no alternate write path. */

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

/* A task filter condition, as `sona work list --filter` reads it. Sona owns
 * which fields, operators and values exist; this only keeps the shape. */
const CONDITION = z
  .object({
    field: z.string(),
    op: z.string(),
    values: z.array(z.union([z.string(), z.number()])),
  })
  .strict();

/* The one shape an argv can be built out of: a flat object of JSON scalars
 * and lists of strings, keyed by field name, plus lists of filter conditions.
 * Nothing here re-checks a value it can turn into an argument — Sona owns
 * what a scope is and what a limit may be — so the schema stops at "flat",
 * which is the only property this file actually relies on. A list of strings
 * is flat too: each entry becomes one argument. A list of conditions becomes
 * one JSON argument. */
export const TOOL_INPUT = z.record(
  z.string(),
  z.union([
    z.string(),
    z.number(),
    z.boolean(),
    z.null(),
    z.array(z.string()),
    z.array(CONDITION),
  ]),
);

export type ToolInput = z.infer<typeof TOOL_INPUT>;

/** The JSON schema of one filter condition, as published. */
const CONDITION_SCHEMA = {
  type: "object",
  properties: {
    field: {
      type: "string",
      enum: [
        "status",
        "priority",
        "owner",
        "project",
        "milestone",
        "label",
        "cycle",
        "due",
        "created",
        "updated",
      ],
      description: "The task field the condition reads.",
    },
    op: {
      type: "string",
      enum: [
        "is",
        "is_not",
        "any_of",
        "none_of",
        "includes_all",
        "includes_any",
        "before",
        "after",
      ],
      description:
        "How it compares. before and after take only due, created and updated. For labels, is and includes_all need every value, any_of and includes_any any, is_not and none_of none.",
    },
    values: {
      type: "array",
      items: { type: ["string", "integer"] },
      description:
        "Ids or names; stored statuses (todo, in_progress, ...); priorities 0-4 or urgent, high, medium, low, none; me for the user; none for an empty field; cycle numbers, current or next; days as YYYY-MM-DD, today, tomorrow, yesterday or epoch ms.",
    },
  },
  required: ["field", "op", "values"],
  additionalProperties: false,
} as const;

/** One JSON-schema property as this server publishes it: a flat string,
 * integer or boolean field, optionally enumerated and bounded, a list of
 * strings, or a list of filter conditions. Typed so readers get the enum
 * without re-parsing the schema. */
export interface ToolProperty {
  readonly type: "string" | "integer" | "boolean" | "array";
  readonly description: string;
  readonly enum?: readonly string[];
  readonly minimum?: number;
  readonly maximum?: number;
  readonly items?: { readonly type: "string" } | typeof CONDITION_SCHEMA;
  readonly minItems?: number;
}

export interface ToolDefinition {
  readonly name: string;
  readonly title: string;
  readonly description: string;
  /** Whether a call changes the user's data, and whether that change takes
   * something away the same tool cannot give back. Absent: read-only. */
  readonly mutates?: "changes" | "destroys";
  /** Published only when `SONA_MCP_TOOLS` asks for it. */
  readonly optIn?: true;
  readonly inputSchema: {
    readonly type: "object";
    readonly properties: Record<string, ToolProperty>;
    readonly required?: readonly string[];
    readonly additionalProperties: false;
  };
  /** The `sona` command line this call becomes. `agent` is the client's
   * name, which every task write passes on so Sona can say who changed it. */
  argv(input: ToolInput, agent?: string): string[];
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

/* `sona work` flags are passed as `--flag=value`, so a title, name or search
 * that starts with a dash stays a value instead of becoming a flag. */
function assigned(flag: string, value: string | null): string[] {
  return value === null ? [] : [`${flag}=${value}`];
}

/** A string where empty is an answer: it clears the field. */
function cleared(flag: string, input: ToolInput, key: string): string[] {
  const value = input[key];
  if (value === undefined || value === null) return [];
  const parsed = z.string().safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError(`${key} must be a string.`);
  }
  return [`${flag}=${parsed.data}`];
}

/** A field where JSON null clears it, spelled the way Sona clears it. */
function nullable(flag: string, input: ToolInput, key: string, clear: string): string[] {
  return input[key] === null ? [`${flag}=${clear}`] : cleared(flag, input, key);
}

/** What every `sona work` write takes: the operation, and who makes it. */
function writing(input: ToolInput, agent: string | undefined): string[] {
  return [
    ...assigned("--operation-id", text(input, "operation_id")),
    ...assigned("--agent", agent ?? null),
  ];
}

function flag(input: ToolInput, key: string): boolean {
  const value = input[key];
  if (value === undefined || value === null) return false;
  const parsed = z.boolean().safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError(`${key} must be true or false.`);
  }
  return parsed.data;
}

/** Task handles or ids, comma- or space-separated, one argument each. */
function tasks(input: ToolInput, key: string): string[] {
  const found = text(input, key, true)
    .split(/[\s,]+/)
    .filter((task) => task !== "");
  if (found.length === 0) throw new SonaInputError(`${key} is required.`);
  return found;
}

/** A list of strings, one argument each, none of them empty. */
function strings(input: ToolInput, key: string): string[] {
  const parsed = z.array(z.string()).safeParse(input[key]);
  if (!parsed.success) {
    throw new SonaInputError(`${key} must be a list of strings.`);
  }
  const found = parsed.data.filter((value) => value.trim() !== "");
  if (found.length === 0) throw new SonaInputError(`${key} is required.`);
  return found;
}

/** Filter conditions, as the one JSON argument `--filter` reads. */
function conditions(input: ToolInput): string[] {
  const value = input.filter;
  if (value === undefined || value === null) return [];
  const parsed = z.array(CONDITION).safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError("filter must be a list of {field, op, values} conditions.");
  }
  return [`--filter=${JSON.stringify(parsed.data)}`];
}

/** A project's links, one `--link` each; null or an empty list clears them. */
function links(input: ToolInput): string[] {
  const value = input.links;
  if (value === undefined) return [];
  const parsed = z.array(z.string()).nullable().safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError("links must be a list of strings.");
  }
  const found = (parsed.data ?? []).filter((link) => link.trim() !== "");
  return found.length === 0 ? ["--link=none"] : found.map((link) => `--link=${link}`);
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
    "The meeting's uuid, as it appears in a sona://meeting/<id> link or in the id field of a sona_search, sona_meetings or sona_meeting_list row.",
} as const;

const PROJECT_ID = {
  type: "string",
  description:
    "The exact saved project uuid returned by sona_project_list or sona_projects. Project titles are not ids.",
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

const WRITES =
  "This changes the user's Sona tasks. It needs Settings > Agents > External access and External mutations; with either off Sona refuses with consent_required and names the setting. Unless the user lets agents' changes apply in Settings > Agents, changes to a task's fields come back under proposed and wait for the user to accept them in Sona, a new task lands in Triage, and triage, duplicates, archiving and label changes are refused.";

const COMMENT_WRITES =
  "This changes the user's Sona tasks. It needs Settings > Agents > External access and External mutations; with either off Sona refuses with consent_required and names the setting.";

const COMMENT = {
  type: "string",
  description:
    "A comment's id, from sona_comment_list or the comments in sona_work_get.",
} as const;

const ATTACHMENT = {
  type: "string",
  description: "An attachment's id, from sona_task_attachments or the attachments in sona_work_get.",
} as const;

const CATALOG_WRITES =
  "This changes the user's Sona projects and people. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. Nothing is proposed for later and there is no dry run. A call that changes nothing answers with operation_id null.";

const PLANNING_WRITES =
  "This changes the user's Sona projects and cycles. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. Nothing is proposed for later. dry_run:true shows the result without writing. Repeating an operation_id answers the first result again.";

const FILTER = {
  type: "array",
  items: CONDITION_SCHEMA,
  description:
    'Conditions that must all hold, on top of the other filters: [{"field":"label","op":"includes_all","values":["bug","ui"]}, {"field":"due","op":"before","values":["today"]}].',
} as const;

const HEALTH = {
  type: "string",
  enum: ["on-track", "at-risk", "off-track"],
  description: "How the project is going.",
} as const;

const RESOLUTION = {
  type: "string",
  enum: ["day", "month", "quarter", "half-year", "year"],
  description: "How exactly the day is meant: a quarter shows as Q3. Sona reads day unless told. null clears it.",
} as const;

const PROJECT = {
  type: "string",
  description: "A project's name or id from sona_project_list.",
} as const;

const PERSON = {
  type: "string",
  description:
    "A person's name, any name they go by, or their id from sona_person_list. Sona refuses a name that matches several.",
} as const;

const TASK = {
  type: "string",
  description: "A task: its handle (S-12), its number, or its id.",
} as const;

const TASKS = {
  type: "string",
  description: "Tasks, comma-separated: handles (S-12), numbers or ids.",
} as const;

const OPERATION_ID = {
  type: "string",
  description:
    "A UUID naming this change so a retry cannot apply it twice. Left out, Sona derives one from the request and the revisions it read, so repeating the same call is already safe.",
} as const;

const DRY_RUN = {
  type: "boolean",
  description: "Return what would change, field by field, and write nothing.",
} as const;

const WORK_SIDE = {
  type: "string",
  enum: ["mine", "waiting", "unowned"],
  description:
    "Whose work: 'mine' is what the user owes, 'waiting' is what others owe the user, 'unowned' is work nobody owns.",
} as const;

const DUE_WINDOWS = {
  type: "string",
  description:
    "Due windows, comma-separated: overdue, today, this-week, next:N for the next N days, or none for work with no due day.",
} as const;

/* The fields a task is created with or changed to. An empty details or
 * labels clears it; every other field takes none to clear. */
const FIELDS = {
  details: {
    type: "string",
    description: "Plain-text details. An empty string clears them.",
  },
  status: {
    type: "string",
    enum: ["triage", "backlog", "todo", "in_progress", "done", "canceled"],
    description: "Where the task stands.",
  },
  priority: {
    type: "string",
    enum: ["none", "urgent", "high", "medium", "low"],
    description: "How urgent it is.",
  },
  owner: {
    type: "string",
    description:
      "me, nobody, or a person's name or id. Look people up with sona_people; Sona refuses a name that matches several.",
  },
  due: {
    type: "string",
    description: "The local day it is due: YYYY-MM-DD, today, tomorrow, or none.",
  },
  repeat: {
    type: "string",
    description:
      "How often it repeats: every monday, every 2 weeks, every weekday or daily; none stops it. Without due, a new task that repeats on named days is due on the next one.",
  },
  project: {
    type: "string",
    description: "A project's name or id from sona_project_list, or none.",
  },
  milestone: {
    type: "string",
    description: "A milestone id in the task's project from sona_milestone_list, or none.",
  },
  labels: {
    type: "string",
    description:
      "Every label the task should have, by name or id, comma-separated; an empty string removes them all. Unknown labels are refused, not created.",
  },
} as const;

function fieldFlags(input: ToolInput): string[] {
  return [
    ...cleared("--details", input, "details"),
    ...assigned("--status", text(input, "status")),
    ...assigned("--priority", text(input, "priority")),
    ...assigned("--owner", text(input, "owner")),
    ...assigned("--due", text(input, "due")),
    ...assigned("--repeat", text(input, "repeat")),
    ...assigned("--project", text(input, "project")),
    ...assigned("--milestone", text(input, "milestone")),
    ...cleared("--labels", input, "labels"),
  ];
}

function scopeFlags(input: ToolInput): string[] {
  const side = text(input, "side");
  if (side !== null && !["mine", "waiting", "unowned"].includes(side)) {
    throw new SonaInputError("side must be mine, waiting or unowned.");
  }
  return [
    ...(side === null ? [] : [`--${side}`]),
    ...assigned("--owner", text(input, "owner")),
    ...assigned("--project", text(input, "project")),
    ...assigned("--label", text(input, "label")),
    ...assigned("--priority", text(input, "priority")),
    ...assigned("--due-by", text(input, "due_by")),
    ...assigned("--due", text(input, "due")),
    ...assigned("--meeting", text(input, "meeting_id")),
    ...assigned("--search", text(input, "search")),
    ...assigned("--cycle", text(input, "cycle")),
  ];
}

/** `archive` and `restore`: the same arguments, opposite directions. */
function archiving(verb: "archive" | "restore", title: string, what: string): ToolDefinition {
  return {
    name: `sona_work_${verb}`,
    title,
    description: `${what} Tasks already that way are listed under skipped. dry_run:true shows the change without writing. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { tasks: TASKS, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["tasks"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      verb,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      ...tasks(input, "tasks"),
    ],
  };
}

/** `relate` and `unrelate`: one link between two tasks. */
function linking(verb: "relate" | "unrelate", title: string, what: string): ToolDefinition {
  return {
    name: `sona_work_${verb}`,
    title,
    description: `${what} task blocks other, task blocked_by other, or task related other. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        link: {
          type: "string",
          enum: ["blocks", "blocked_by", "related"],
          description: "How task stands to other.",
        },
        other: TASK,
        operation_id: OPERATION_ID,
      },
      required: ["task", "link", "other"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      verb,
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
      text(input, "link", true),
      text(input, "other", true),
    ],
  };
}

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
      "Read the numbers a person's page in Sona leads with: confirmed and suggested meeting counts, time recorded together, when you last met, their average talk share in permille, ledger outcomes (landed, open, dropped), open loops you owe and owed to you and how many of them came up in more than one meeting of a series (carried_over), plans heard naming this person whose time passed with no recording of them (plans_without_record; sona_plans lists them), confirmed meetings in each of the last 52 local weeks (oldest first, weeks start Monday, null before the corpus's first meeting) and the usual gap in days between meeting days. Suggested links count only as suggested. Unavailable numbers are null, not zero. No transcript text, titles or names. Requires external access; no writes.",
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
    name: "sona_bookmarks",
    title: "List bookmarks pressed while recording",
    description:
      "List the bookmarks pressed during recordings, newest meeting first: each one's kind (mark, decision, question or todo), its label if one was typed, how far into the recording it was pressed, and a sona://meeting link that opens at that moment. Meetings from a series kept on this Mac are left out. Requires external access; no writes.",
    inputSchema: {
      type: "object",
      properties: { limit: LIMIT },
      additionalProperties: false,
    },
    argv: (input) => [
      "--bookmarks",
      ...optional("--limit", count(input, "limit")),
    ],
  },
  {
    name: "sona_roster",
    title: "Read the names a call listed",
    description:
      "Read the names of the other people a meeting's call app listed while it was recorded, as the call showed them, and which app they came from (null when names were not read). Names are never linked to people here. A meeting from a series kept on this Mac is refused as not found. Requires external access; no writes.",
    inputSchema: {
      type: "object",
      properties: { meeting_id: MEETING_ID },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input) => ["--roster", text(input, "meeting_id", true)],
  },
  {
    name: "sona_place",
    title: "Read where a meeting was recorded",
    description:
      "Read the place set for a meeting (name, latitude, longitude; null when none was set) and the other recordings within 100 m of it, newest first. A meeting from a series kept on this Mac is refused as not found and never listed nearby. Requires external access; no writes and no map lookups.",
    inputSchema: {
      type: "object",
      properties: { meeting_id: MEETING_ID, limit: LIMIT },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input) => [
      "--place",
      text(input, "meeting_id", true),
      ...optional("--limit", count(input, "limit")),
    ],
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
    name: "sona_project_get",
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
    mutates: "changes",
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
  {
    name: "sona_work_list",
    title: "List Sona tasks",
    description:
      "List tasks in Sona's tracker, most urgent first: priority, then due day. Without status it lists triage and open work that is neither archived nor snoozed. ready:true lists only Todo tasks that nothing open blocks and that are not snoozed, the work to pick up next, and takes only the side, owner, project, label, priority, due_by, due, meeting_id, search, cycle and limit filters. Each task has a handle (S-12) that works wherever a task is asked for, its id, status, priority, owner, due_date, follow_up_at_ms for work someone else owes, focus when it is one of today's focus picks, project, labels, cycle, revision and, for work raised in a meeting, the quote and a sona://loop link. at_risk marks the user's work due within two days with no free hour left on its due day; repeat says how a repeating task comes back and how many of its instances were done in time. Work from meetings kept on this Mac is left out. Read total and has_more before treating the list as complete.",
    inputSchema: {
      type: "object",
      properties: {
        ready: {
          type: "boolean",
          description:
            "List only the Todo tasks that are ready to start: unblocked and not snoozed.",
        },
        status: {
          type: "string",
          description:
            "Comma-separated: triage, backlog, todo, in_progress, done, canceled, duplicate, open, closed or all.",
        },
        side: WORK_SIDE,
        owner: {
          type: "string",
          description:
            "People by name or id, comma-separated. me or nobody work too, on their own and without side.",
        },
        project: {
          type: "string",
          description:
            "Projects by name or id, comma-separated; none for work in no project.",
        },
        label: {
          type: "string",
          description: "Labels by name or id, comma-separated: work with any of them.",
        },
        priority: {
          type: "string",
          description: "Comma-separated: urgent, high, medium, low or none.",
        },
        due_by: {
          type: "string",
          description: "Due on or before this local day: YYYY-MM-DD, today or tomorrow.",
        },
        due: DUE_WINDOWS,
        meeting_id: {
          ...MEETING_ID,
          description: "Only work raised in this meeting. " + MEETING_ID.description,
        },
        search: {
          type: "string",
          description: "Words in the title or details, or a task handle such as S-12.",
        },
        cycle: {
          type: "string",
          description: "Only work planned in this cycle: current, next, its number, or none.",
        },
        view: {
          type: "string",
          description:
            "Start from a saved view's filters, by name or id; the other filters narrow it.",
        },
        archived: { type: "boolean", description: "Include archived work." },
        snoozed: { type: "boolean", description: "Include snoozed work." },
        filter: FILTER,
        limit: LIMIT,
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      flag(input, "ready") ? "ready" : "list",
      ...scopeFlags(input),
      ...assigned("--status", text(input, "status")),
      ...assigned("--view", text(input, "view")),
      ...(flag(input, "archived") ? ["--archived"] : []),
      ...(flag(input, "snoozed") ? ["--snoozed"] : []),
      ...conditions(input),
      ...assigned("--limit", count(input, "limit")),
      "--json",
    ],
  },
  {
    name: "sona_work_get",
    title: "Read a Sona task",
    description:
      "Read one task: every field, how often it repeats, the meeting quote it came from with its speaker and a sona://loop link, the tasks it blocks, is blocked by or relates to, the task it sits under (parent) and its sub-tasks with how many are done (sub_tasks), its comment threads with replies, its attachments (files with the path of Sona's copy, and links), its evidence (meetings, notes, dictation quotes and links), what Sona suggests for it while it waits in triage (owner, due day, project, duplicate) with the reason and quote, and its history, each entry marked as made by the user (me), Sona or an agent.",
    inputSchema: {
      type: "object",
      properties: { task: TASK },
      required: ["task"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "show", "--json", "--", text(input, "task", true)],
  },
  {
    name: "sona_work_follow_up",
    title: "Draft a meeting's follow-up",
    description:
      "Read who owes what from one meeting, by when, with the words each promise was said in: the open and triage tasks the meeting raised, grouped by owner, and the follow-up message ready to send. Sona sends nothing.",
    inputSchema: {
      type: "object",
      properties: {
        meeting: { type: "string", description: "The meeting's id." },
      },
      required: ["meeting"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "follow-up", "--json", "--", text(input, "meeting", true)],
  },
  {
    name: "sona_work_counts",
    title: "Read my work in Sona",
    description:
      "Read the tracker's counts (triage waiting, your open work, overdue, due today, waiting on others and how many of those are due a follow-up), then the open tasks the user owes and the open tasks others owe the user, most urgent first.",
    inputSchema: {
      type: "object",
      properties: {
        limit: {
          ...LIMIT,
          description: "How many tasks to return per list. Sona defaults to 25 and caps at 100.",
        },
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "mine",
      ...assigned("--limit", count(input, "limit")),
      "--json",
    ],
  },
  {
    name: "sona_work_create",
    title: "Create a Sona task",
    description: `Create a task: Todo, no priority and no owner unless told otherwise. ${WRITES} Calling again with the same arguments on the same day returns the first task with replayed:true instead of a second one; pass a new operation_id to create a twin on purpose.`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        title: { type: "string", description: "The task's title." },
        ...FIELDS,
        operation_id: OPERATION_ID,
      },
      required: ["title"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "create",
      ...fieldFlags(input),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "title", true),
    ],
  },
  {
    name: "sona_work_update",
    title: "Change Sona tasks",
    description: `Change one task or several. Several tasks take the same change together or not at all; add_labels and remove_labels go task by task. Fields left out stay as they are, and tasks already as asked are reported under skipped. dry_run:true returns each field's from and to, and whether a meeting commitment would close or reopen, without writing. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        tasks: TASKS,
        title: { type: "string", description: "A new title." },
        ...FIELDS,
        add_labels: {
          type: "string",
          description: "Labels to add, by name or id, comma-separated.",
        },
        remove_labels: {
          type: "string",
          description: "Labels to remove, by name or id, comma-separated.",
        },
        parent: {
          type: "string",
          description:
            "Put the tasks under another task (handle, number or id); null or none lifts them back to top level. A task cannot go under itself or one of its own sub-tasks.",
        },
        snooze_until: {
          type: "string",
          description:
            "Hide the task until this local day: YYYY-MM-DD, today, tomorrow, or none to wake it.",
        },
        cycle: {
          type: "string",
          description:
            "Plan the tasks in a cycle: current, next, its number, or none to take them out.",
        },
        expected_revision: {
          type: "integer",
          description:
            "Refuse unless the one task named is still at this revision, from a read made before deciding the change.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["tasks"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "update",
      ...assigned("--title", text(input, "title")),
      ...fieldFlags(input),
      ...assigned("--add-label", text(input, "add_labels")),
      ...assigned("--remove-label", text(input, "remove_labels")),
      ...assigned("--snooze-until", text(input, "snooze_until")),
      ...assigned("--cycle", text(input, "cycle")),
      ...nullable("--parent", input, "parent", "none"),
      ...assigned("--expected-revision", count(input, "expected_revision")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      ...tasks(input, "tasks"),
    ],
  },
  {
    name: "sona_work_claim",
    title: "Start a Sona task",
    description: `Start a Todo or Backlog task before working on it: it moves to In progress with you shown working it, and its owner stays who it was. Claiming a task you already work changes nothing. Refused when another agent works it, or when it is in progress without you, still in triage, archived or closed, so two agents cannot both start it. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: { task: TASK, operation_id: OPERATION_ID },
      required: ["task"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "claim",
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
    ],
  },
  {
    name: "sona_work_done",
    title: "Finish Sona tasks",
    description: `Mark tasks done. The meeting commitment a task came from closes with it. evidence keeps a link, such as a pull request, with the task as evidence; any other line is added to its details. dry_run:true shows the change without writing. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        tasks: TASKS,
        evidence: {
          type: "string",
          description:
            "A link (http, https or file) showing the work is done, kept with the task as evidence, or one line added to its details. One task only.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["tasks"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "done",
      ...assigned("--evidence", text(input, "evidence")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      ...tasks(input, "tasks"),
    ],
  },
  {
    name: "sona_subtask_create",
    title: "Make Sona sub-tasks",
    description: `Make one sub-task under a task for each title. Each takes the parent's project, milestone and priority unless priority is given; the owner, left out, is the user when the parent is theirs, else the owner its other sub-tasks share, else nobody. dry_run:true shows them without writing. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        parent: { ...TASK, description: `The parent. ${TASK.description}` },
        titles: {
          type: "array",
          items: { type: "string" },
          minItems: 1,
          description: "One title per sub-task.",
        },
        owner: FIELDS.owner,
        priority: FIELDS.priority,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["parent", "titles"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "subtask",
      ...assigned("--owner", text(input, "owner")),
      ...assigned("--priority", text(input, "priority")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "parent", true),
      ...strings(input, "titles"),
    ],
  },
  {
    name: "sona_comment_list",
    title: "Read a Sona task's comments",
    description:
      "Read a task's comment threads, oldest first: each comment's id, author (me, sona or an agent by name), Markdown body, when it was written, edited and resolved, and its replies. A deleted comment keeps its place with body null while it has replies. Pass next_cursor back as cursor for the next page.",
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        limit: {
          ...LIMIT,
          description: "How many threads to return. Sona defaults to 50 and caps at 100.",
        },
        cursor: {
          type: "string",
          description: "The next_cursor of the page before.",
        },
      },
      required: ["task"],
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "comments",
      ...assigned("--limit", count(input, "limit")),
      ...assigned("--cursor", text(input, "cursor")),
      "--json",
      "--",
      text(input, "task", true),
    ],
  },
  {
    name: "sona_comment_save",
    title: "Comment on a Sona task",
    description: `Without id, add a comment to task, or with reply_to answer in that comment's thread (a reply to a reply joins the thread's first comment). With id, change the body of a comment you wrote; task and reply_to are then left out. The comment is shown as yours. Use it to say what you are doing on a task you claimed, what you found and what is left. dry_run:true shows it without writing. ${COMMENT_WRITES} Agents may comment whether their changes apply or wait for the user.`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        id: { ...COMMENT, description: `The comment to change. ${COMMENT.description}` },
        task: { ...TASK, description: `The task to comment on, when id is left out. ${TASK.description}` },
        reply_to: {
          ...COMMENT,
          description: `Reply in this comment's thread. ${COMMENT.description}`,
        },
        body: { type: "string", description: "The comment, in Markdown." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["body"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const id = text(input, "id");
      const body = text(input, "body", true);
      const dryRun = flag(input, "dry_run") ? ["--dry-run"] : [];
      if (id !== null) {
        if (text(input, "task") !== null || text(input, "reply_to") !== null) {
          throw new SonaInputError("id changes a comment; leave task and reply_to out.");
        }
        return [
          "work",
          "comment-edit",
          `--body=${body}`,
          ...dryRun,
          ...writing(input, agent),
          "--json",
          "--",
          id,
        ];
      }
      return [
        "work",
        "comment",
        "add",
        `--body=${body}`,
        ...assigned("--reply-to", text(input, "reply_to")),
        ...dryRun,
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "task", true),
      ];
    },
  },
  {
    name: "sona_comment_resolve",
    title: "Resolve a Sona comment thread",
    description: `Resolve the thread a comment is in, or reopen it with reopen:true. Any comment in the thread names it. dry_run:true shows the change without writing. ${COMMENT_WRITES} Unless the user lets agents' changes apply in Settings > Agents, Sona may refuse it.`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        id: COMMENT,
        reopen: { type: "boolean", description: "Reopen a resolved thread instead." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "comment-resolve",
      ...(flag(input, "reopen") ? ["--reopen"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "id", true),
    ],
  },
  {
    name: "sona_comment_delete",
    title: "Delete a Sona comment",
    description: `Delete a comment you wrote. Its body is gone; replies under it stay, under a deleted marker. dry_run:true shows the comment without deleting it. ${COMMENT_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        id: COMMENT,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "comment-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "id", true),
    ],
  },
  {
    name: "sona_task_attach",
    title: "Attach a file or link to a Sona task",
    description: `Attach a file or a link to a task: exactly one of path or url. A file is copied into Sona (at most 50 MB); a link must start with http://, https://, mailto: or sona://. Attaching the same file or link to the same task again answers the one already there with replayed:true. Attach what you made for the task, such as a report, a screenshot or a pull request link; never attach secrets, keys, tokens or credentials. dry_run:true checks the file or link and writes nothing. ${COMMENT_WRITES} Agents may attach whether their changes apply or wait for the user.`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        path: {
          type: "string",
          description: "The absolute path of a file to copy in. A folder is refused.",
        },
        url: {
          type: "string",
          description: "A link: http, https, mailto or sona.",
        },
        title: {
          type: "string",
          description: "Shown instead of the file's name or the link.",
        },
        subtitle: { type: "string", description: "A second line under the title." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const path = text(input, "path");
      const url = text(input, "url");
      if ((path === null) === (url === null)) {
        throw new SonaInputError("Give exactly one of path or url.");
      }
      return [
        "work",
        "attach",
        ...assigned("--title", text(input, "title")),
        ...assigned("--subtitle", text(input, "subtitle")),
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "task", true),
        path ?? url ?? "",
      ];
    },
  },
  {
    name: "sona_task_attachments",
    title: "Read a Sona task's attachments",
    description:
      "Read a task's attachments, oldest first: each one's id, kind (file, image or link), title, subtitle, the link or the absolute path of Sona's copy of the file, its original name, type, size and sha256, when it was attached and by whom (me, sona or an agent by name).",
    inputSchema: {
      type: "object",
      properties: { task: TASK },
      required: ["task"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "attachments", "--json", "--", text(input, "task", true)],
  },
  {
    name: "sona_task_detach",
    title: "Remove an attachment from a Sona task",
    description: `Remove an attachment you attached; Sona's copy of a file is deleted with it. dry_run:true shows the attachment without removing it. ${COMMENT_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        id: ATTACHMENT,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task", "id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "detach",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
      text(input, "id", true),
    ],
  },
  {
    name: "sona_work_triage",
    title: "Triage Sona tasks",
    description: `Decide on work Sona pulled out of meetings. accept moves triage items to Todo with Sona's suggested owner, due day and project unless ignore_suggestions is true. decline cancels them and sets their meeting commitments aside. duplicate marks one task a duplicate of another (of), moving its quotes and links there. Items not in triage are left alone and listed under skipped. dry_run:true previews accept and decline. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        tasks: {
          ...TASKS,
          description: `${TASKS.description} duplicate takes one.`,
        },
        decision: {
          type: "string",
          enum: ["accept", "decline", "duplicate"],
          description: "What to do with them.",
        },
        of: {
          ...TASK,
          description: `For duplicate: the task they repeat. ${TASK.description}`,
        },
        ignore_suggestions: {
          type: "boolean",
          description: "For accept: leave owner, due day and project as they are.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["tasks", "decision"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const decision = text(input, "decision", true);
      const common = [
        ...writing(input, agent),
        "--json",
      ];
      switch (decision) {
        case "accept":
          return [
            "work",
            "accept",
            ...(flag(input, "ignore_suggestions") ? ["--ignore-suggestions"] : []),
            ...(flag(input, "dry_run") ? ["--dry-run"] : []),
            ...common,
            "--",
            ...tasks(input, "tasks"),
          ];
        case "decline":
          return [
            "work",
            "decline",
            ...(flag(input, "dry_run") ? ["--dry-run"] : []),
            ...common,
            "--",
            ...tasks(input, "tasks"),
          ];
        case "duplicate":
          return [
            "work",
            "duplicate",
            `--of=${text(input, "of", true)}`,
            ...common,
            "--",
            ...tasks(input, "tasks"),
          ];
        default:
          throw new SonaInputError("decision must be accept, decline or duplicate.");
      }
    },
  },
  archiving("archive", "Archive Sona tasks", "Hide tasks from every list without changing their status."),
  archiving("restore", "Restore Sona tasks", "Bring archived tasks back into their lists."),
  linking("relate", "Link Sona tasks", "Link two tasks. Linking them again returns the link already there."),
  linking("unrelate", "Unlink Sona tasks", "Remove a link between two tasks."),
  {
    name: "sona_cycle_list",
    title: "Read Sona cycles",
    description:
      "Read the tracker's cycles: how many weeks each runs, the weekday it starts, the cooldown weeks between cycles and how many are planned ahead, then every cycle with its number, name, state (past, current or upcoming), first and last day, scope (tasks planned in it and not dropped), started, done and, once ended, success (done counts 100, started 25, over scope). With cycle, read that one cycle's scope, started and done as each of its days ended, and how many tasks were carried over unfinished from earlier cycles. Unfinished work moves to the new cycle by itself when one starts.",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        cycle: {
          type: "string",
          description: "One cycle to read day by day: current, next, or its number.",
        },
      },
      additionalProperties: false,
    },
    argv: (input) => {
      const cycle = text(input, "cycle");
      return cycle === null
        ? ["work", "cycles", "--json"]
        : ["work", "cycle", "--json", "--", cycle];
    },
  },
  {
    name: "sona_cycle_save",
    title: "Change Sona cycles",
    description: `Change how cycles run, or one cycle. One kind of change per call: weeks, start_weekday, cooldown_weeks and upcoming set how every cycle runs (leave cycle out; fields left out stay); name, start and end rename or move one cycle; start_today:true starts the first upcoming cycle, or cycle, today; rollover:true moves cycle's unfinished work into the cycle under way. Answers with every cycle as sona_cycle_list reads them. ${PLANNING_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        cycle: {
          type: "string",
          description: "The cycle to change: current, next, or its number.",
        },
        name: {
          type: "string",
          description: "The cycle's name. null clears it.",
        },
        start: {
          type: "string",
          description: "The cycle's first day, YYYY-MM-DD. Only a cycle not yet started moves.",
        },
        end: { type: "string", description: "The cycle's last day, YYYY-MM-DD." },
        start_today: { type: "boolean", description: "Start the cycle today." },
        rollover: {
          type: "boolean",
          description: "Move the cycle's unfinished work into the cycle under way.",
        },
        weeks: {
          type: "integer",
          minimum: 1,
          maximum: 8,
          description: "How many weeks every cycle runs.",
        },
        start_weekday: {
          type: "integer",
          minimum: 1,
          maximum: 7,
          description: "The weekday cycles start on: 1 for Monday through 7 for Sunday.",
        },
        cooldown_weeks: {
          type: "integer",
          minimum: 0,
          maximum: 4,
          description: "Weeks off between cycles.",
        },
        upcoming: {
          type: "integer",
          minimum: 1,
          maximum: 15,
          description: "How many cycles to plan ahead.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const cycle = text(input, "cycle");
      const settings = [
        ...assigned("--weeks", count(input, "weeks")),
        ...assigned("--start-weekday", count(input, "start_weekday")),
        ...assigned("--cooldown", count(input, "cooldown_weeks")),
        ...assigned("--upcoming", count(input, "upcoming")),
      ];
      const edit = [
        ...nullable("--name", input, "name", "none"),
        ...assigned("--start", text(input, "start")),
        ...assigned("--end", text(input, "end")),
      ];
      const startToday = flag(input, "start_today");
      const rollover = flag(input, "rollover");
      const kinds = [settings.length > 0, edit.length > 0, startToday, rollover];
      if (kinds.filter(Boolean).length > 1) {
        throw new SonaInputError(
          "Pass one kind of change: settings (weeks, start_weekday, cooldown_weeks, upcoming), name/start/end, start_today or rollover.",
        );
      }
      const tail = [
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
      ];
      if (settings.length > 0) {
        if (cycle !== null) {
          throw new SonaInputError(
            "weeks, start_weekday, cooldown_weeks and upcoming apply to every cycle; leave cycle out.",
          );
        }
        return ["work", "cycle-settings", ...settings, ...tail];
      }
      if (startToday) {
        return ["work", "cycle-start-today", ...tail, ...(cycle === null ? [] : ["--", cycle])];
      }
      if (cycle === null) throw new SonaInputError("cycle is required.");
      return rollover
        ? ["work", "cycle-rollover", ...tail, "--", cycle]
        : ["work", "cycle-save", ...edit, ...tail, "--", cycle];
    },
  },
  {
    name: "sona_work_labels",
    title: "List Sona labels",
    description: "List the tracker's labels with their ids, colors (hue in degrees) and revisions.",
    optIn: true,
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "labels", "--json"],
  },
  {
    name: "sona_work_label_save",
    title: "Save a Sona label",
    description: `Create a label, or rename or recolor one. Saving a name that exists changes that label; label renames another label to name. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The label's name." },
        label: {
          type: "string",
          description: "The label to rename to name, by name or id.",
        },
        hue: {
          type: "integer",
          minimum: 0,
          maximum: 360,
          description: "Its color as degrees on the color wheel. Chosen from the name when left out.",
        },
        operation_id: OPERATION_ID,
      },
      required: ["name"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "label-save",
      ...assigned("--label", text(input, "label")),
      ...assigned("--hue", count(input, "hue")),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "name", true),
    ],
  },
  {
    name: "sona_work_label_delete",
    title: "Delete a Sona label",
    description: `Delete a label. Its tasks lose only that label. ${WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        label: { type: "string", description: "The label's name or id." },
        operation_id: OPERATION_ID,
      },
      required: ["label"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "label-delete",
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "label", true),
    ],
  },
  {
    name: "sona_work_views",
    title: "List saved Sona views",
    description: "List saved task views with their filters, layout, grouping and order.",
    optIn: true,
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "views", "--json"],
  },
  {
    name: "sona_work_view_save",
    title: "Save a Sona view",
    description: `Save named task filters the app shows as a view and sona_work_list reads with view. Saving a name that exists changes that view; filters given replace the ones it had. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The view's name." },
        view: {
          type: "string",
          description: "The view to rename to name, by name or id.",
        },
        status: {
          type: "string",
          description:
            "Comma-separated: triage, backlog, todo, in_progress, done, canceled, duplicate, open, closed or all.",
        },
        side: WORK_SIDE,
        owner: {
          type: "string",
          description:
            "People by name or id, comma-separated. me or nobody work too, on their own and without side.",
        },
        project: { type: "string", description: "Projects by name or id, comma-separated." },
        label: { type: "string", description: "Labels by name or id, comma-separated." },
        priority: {
          type: "string",
          description: "Comma-separated: urgent, high, medium, low or none.",
        },
        due: DUE_WINDOWS,
        search: { type: "string", description: "Words in the title or details." },
        layout: { type: "string", enum: ["list", "board"], description: "How the app shows it." },
        group_by: {
          type: "string",
          enum: ["status", "owner", "project", "meeting", "due", "priority", "none"],
          description: "How the app groups it.",
        },
        order_by: {
          type: "string",
          enum: ["priority", "due", "updated", "created", "manual"],
          description: "How the app orders it.",
        },
        filter: FILTER,
        operation_id: OPERATION_ID,
      },
      required: ["name"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "view-save",
      ...assigned("--view", text(input, "view")),
      ...assigned("--status", text(input, "status")),
      ...scopeFlags(input),
      ...assigned("--layout", text(input, "layout")),
      ...assigned("--group-by", text(input, "group_by")),
      ...assigned("--order-by", text(input, "order_by")),
      ...conditions(input),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "name", true),
    ],
  },
  {
    name: "sona_work_view_delete",
    title: "Delete a saved Sona view",
    description: `Delete a saved view. No task changes. ${WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        view: { type: "string", description: "The view's name or id." },
        operation_id: OPERATION_ID,
      },
      required: ["view"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "view-delete",
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "view", true),
    ],
  },
  {
    name: "sona_work_ask",
    title: "Ask Sona's conversations",
    description:
      "Ask a question over every retained meeting, what meetings left open, and dictations: \"what did Dana promise about pricing\", \"what did we decide on onboarding last week\". Returns the lines that answer it, numbered, each with who said it, where, when and a sona:// link, and read_as: the words, person and time the question was read as. Lines, not a written answer; meetings kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: {
        question: { type: "string", description: "The question, in plain words." },
      },
      required: ["question"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "ask", "--json", "--", text(input, "question", true)],
  },
  {
    name: "sona_work_take",
    title: "Make tasks from Sona answer lines",
    description: `Ask the same question as sona_work_ask and make Triage tasks of the lines numbered in lines. Each task keeps its quote and where it was said; a line about something a meeting left open returns the task already following it. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        question: {
          type: "string",
          description: "The question sona_work_ask answered, word for word.",
        },
        lines: {
          type: "string",
          description: "Line numbers from that answer, comma-separated: 1,3.",
        },
        operation_id: OPERATION_ID,
      },
      required: ["question", "lines"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "ask",
      `--take=${tasks(input, "lines").join(",")}`,
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "question", true),
    ],
  },
  {
    name: "sona_work_say",
    title: "Change Sona tasks in words",
    description: `Change tasks by saying the change: "push everything not urgent to monday", "move Dana's tasks to next week", "mark everything in triage for launch done", "archive canceled tasks". Sona reads it with a fixed grammar, never a model, and refuses words it cannot read. Without a who, it means the user's own open work. dry_run:true returns each task's from and to without writing; run it first. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        words: { type: "string", description: "The change, in plain words." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["words"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "say",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "words", true),
    ],
  },
  {
    name: "sona_work_watches",
    title: "List Sona watches",
    description:
      "List the topics Sona listens for, each with the last lines that said it in a meeting or dictation.",
    optIn: true,
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "watches", "--json"],
  },
  {
    name: "sona_work_watch",
    title: "Watch a topic in Sona",
    description: `Listen for a word or phrase: when a processed meeting or a dictation says it, Sona notifies the user once, with the line. person limits it to what one person says. stop:true stops watching it. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        topic: { type: "string", description: "The word or phrase." },
        person: {
          type: "string",
          description: "Only when this person says it: a name or id from sona_people.",
        },
        stop: { type: "boolean", description: "Stop watching topic." },
      },
      required: ["topic"],
      additionalProperties: false,
    },
    argv: (input) =>
      flag(input, "stop")
        ? ["work", "unwatch", "--json", "--", text(input, "topic", true)]
        : [
            "work",
            "watch",
            ...assigned("--person", text(input, "person")),
            "--json",
            "--",
            text(input, "topic", true),
          ],
  },
  {
    name: "sona_project_list",
    title: "List Sona projects",
    description:
      "List the projects the Projects page shows, by name: each one's id, lead, members, status (backlog, planned, active, paused, completed or canceled), priority, start and target days, colour, icon, labels, links, open and done task counts, milestone count, whether it was put away, and revision. all:true includes projects put away; trashed:true lists only the projects in the bin, deleted less than 30 days ago.",
    inputSchema: {
      type: "object",
      properties: {
        all: { type: "boolean", description: "Include projects that were put away." },
        trashed: {
          type: "boolean",
          description:
            "List only the projects in the bin, which sona_project_delete restore:true brings back.",
        },
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "projects",
      ...(flag(input, "all") ? ["--all"] : []),
      ...(flag(input, "trashed") ? ["--trashed"] : []),
      "--json",
    ],
  },
  {
    name: "sona_project_detail",
    title: "Read a Sona project's progress",
    description:
      "Read one project from the Projects page: its fields, the latest update with its health, update_missing when an active project has gone a cycle and three days without one, progress (scope, started, done, percent complete with done counting 100 and started 25, weekly history, and the predicted finish with optimistic and pessimistic days, null until there is enough history), and each milestone's progress in order, with next_open on the first one not complete.",
    inputSchema: {
      type: "object",
      properties: { project: PROJECT },
      required: ["project"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "project", "--json", "--", text(input, "project", true)],
  },
  {
    name: "sona_project_save",
    title: "Save a Sona project",
    description: `Make a project, or change one. Saving a name no project has makes it; saving a name one has changes that one; project renames another project to name. Fields left out stay as they are; null clears one. ${PLANNING_WRITES} A call that changes nothing answers with operation_id null.`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The project's name." },
        project: {
          ...PROJECT,
          description: `The project to rename to name. ${PROJECT.description}`,
        },
        details: {
          type: "string",
          description: "Plain-text details. An empty string or null clears them.",
        },
        status: {
          type: "string",
          enum: ["backlog", "planned", "active", "paused", "completed", "canceled"],
          description: "Where the project stands.",
        },
        priority: {
          type: "string",
          enum: ["none", "urgent", "high", "medium", "low"],
          description: "How urgent it is. null clears it.",
        },
        lead: {
          ...PERSON,
          description: `Who leads it. ${PERSON.description} none or null clears it.`,
        },
        members: {
          type: "string",
          description:
            "Every member, comma-separated, each as a name, alias or id from sona_person_list. Replaces the list; an empty string or null clears it.",
        },
        start: {
          type: "string",
          description:
            "The local day work starts: YYYY-MM-DD, today, tomorrow, or none or null to clear it.",
        },
        start_resolution: RESOLUTION,
        target: {
          type: "string",
          description: "The local day it should be done: YYYY-MM-DD, today, tomorrow, or none or null to clear it.",
        },
        target_resolution: RESOLUTION,
        hue: {
          type: "integer",
          minimum: 0,
          maximum: 360,
          description: "Its colour as degrees on the colour wheel.",
        },
        icon: {
          type: "string",
          description: "A short symbol or emoji. none or null clears it.",
        },
        labels: {
          type: "string",
          description:
            "Every label, by name or id from sona_work_labels, comma-separated. Replaces the list; an empty string or null clears it.",
        },
        links: {
          type: "array",
          items: { type: "string" },
          description:
            "Every link, each TITLE=URL or a bare URL. Replaces the list; an empty list or null clears it.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["name"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "project-save",
      ...assigned("--project", text(input, "project")),
      ...nullable("--details", input, "details", ""),
      ...assigned("--status", text(input, "status")),
      ...nullable("--priority", input, "priority", "none"),
      ...nullable("--lead", input, "lead", "none"),
      ...nullable("--members", input, "members", ""),
      ...nullable("--start", input, "start", "none"),
      ...nullable("--start-resolution", input, "start_resolution", "none"),
      ...nullable("--target", input, "target", "none"),
      ...nullable("--target-resolution", input, "target_resolution", "none"),
      ...assigned("--hue", count(input, "hue")),
      ...nullable("--icon", input, "icon", "none"),
      ...nullable("--labels", input, "labels", ""),
      ...links(input),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "name", true),
    ],
  },
  {
    name: "sona_project_delete",
    title: "Put a Sona project in the bin",
    description: `Put a project in the bin, or take it back out with restore:true. It leaves every list; its tasks stay, in no project. Thirty days later Sona deletes it for good with its milestones, links, labels and updates. sona_project_list trashed:true lists the bin. A project already that way is answered unchanged. ${PLANNING_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        project: PROJECT,
        restore: { type: "boolean", description: "Take the project back out of the bin." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["project"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "project-delete",
      ...(flag(input, "restore") ? ["--restore"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "project", true),
    ],
  },
  {
    name: "sona_project_update_list",
    title: "Read a Sona project's updates",
    description:
      "Read a project's posted updates, newest first: each one's id, health (on_track, at_risk or off_track), Markdown body, percent complete when it was posted, author (me or an agent by name), and when it was posted and edited. Pass next_cursor back as cursor for the next page.",
    inputSchema: {
      type: "object",
      properties: {
        project: PROJECT,
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 100,
          description: "How many updates to return. Sona defaults to 20 and caps at 100.",
        },
        cursor: { type: "string", description: "next_cursor from the page before." },
      },
      required: ["project"],
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "project-updates",
      ...assigned("--limit", count(input, "limit")),
      ...assigned("--cursor", text(input, "cursor")),
      "--json",
      "--",
      text(input, "project", true),
    ],
  },
  {
    name: "sona_project_update_save",
    title: "Post a Sona project update",
    description: `Without id, post an update on project: its health and either a Markdown body or draft:true, which has Sona write it from what moved since the last update (tasks completed and started, milestones reached, the percent change); with dry_run:true it shows that draft without posting. With id, change the health or body of an update you posted; project and draft are then left out. ${PLANNING_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        project: PROJECT,
        id: {
          type: "string",
          description: "The update to change, by its id from sona_project_update_list.",
        },
        health: HEALTH,
        body: { type: "string", description: "The update, in Markdown." },
        draft: { type: "boolean", description: "Post the update Sona drafts instead of a body." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const id = text(input, "id");
      const body = assigned("--body", text(input, "body"));
      const tail = [
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
      ];
      if (id !== null) {
        if (text(input, "project") !== null || flag(input, "draft")) {
          throw new SonaInputError("With id, leave project and draft out.");
        }
        return [
          "work",
          "project-update-edit",
          ...assigned("--health", text(input, "health")),
          ...body,
          ...tail,
          id,
        ];
      }
      const project = text(input, "project", true);
      const health = text(input, "health", true);
      if (body.length > 0 === flag(input, "draft")) {
        throw new SonaInputError("Pass exactly one of body or draft:true.");
      }
      return ["work", "project-update", `--health=${health}`, ...body, ...tail, project];
    },
  },
  {
    name: "sona_project_update_delete",
    title: "Delete a Sona project update",
    description: `Delete an update you posted. No tool brings it back. dry_run:true shows the update without deleting it. ${PLANNING_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        id: {
          type: "string",
          description: "The update, by its id from sona_project_update_list.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "project-update-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "id", true),
    ],
  },
  {
    name: "sona_project_archive",
    title: "Put a Sona project away",
    description: `Put a project away so it leaves the Projects page and sona_project_list, or bring it back with restore:true. Its tasks and milestones stay. A project already that way is answered unchanged. ${CATALOG_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        project: PROJECT,
        restore: { type: "boolean", description: "Bring a put-away project back." },
        operation_id: OPERATION_ID,
      },
      required: ["project"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "project-archive",
      ...(flag(input, "restore") ? ["--restore"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "project", true),
    ],
  },
  {
    name: "sona_milestone_list",
    title: "List a Sona project's milestones",
    description:
      "List one project's milestones in their order: each one's id, name, details, status, target day, open and done task counts, and revision.",
    inputSchema: {
      type: "object",
      properties: { project: PROJECT },
      required: ["project"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "milestones", "--json", "--", text(input, "project", true)],
  },
  {
    name: "sona_milestone_reorder",
    title: "Order a Sona project's milestones",
    description: `Put a project's milestones in a new order. milestones names every one of them, by name or id from sona_milestone_list, first to last. ${PLANNING_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        project: PROJECT,
        milestones: {
          type: "array",
          items: { type: "string" },
          minItems: 1,
          description: "Every milestone of the project, first to last.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["project", "milestones"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "milestone-reorder",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "project", true),
      ...strings(input, "milestones"),
    ],
  },
  {
    name: "sona_milestone_save",
    title: "Save a Sona milestone",
    description: `Make a milestone in a project, or change one. Saving a name the project has no milestone by makes it; saving one it has changes that one; milestone renames another milestone to name. Fields left out stay as they are. ${CATALOG_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        project: PROJECT,
        name: { type: "string", description: "The milestone's name." },
        milestone: {
          type: "string",
          description: "The milestone to rename to name, by name or id from sona_milestone_list.",
        },
        target: {
          type: "string",
          description: "The local day it should be reached: YYYY-MM-DD, today, tomorrow, or none or null to clear it.",
        },
        details: {
          type: "string",
          description: "Plain-text details. An empty string or null clears them.",
        },
        status: {
          type: "string",
          enum: ["not-started", "in-progress", "done", "canceled"],
          description: "Where the milestone stands.",
        },
        operation_id: OPERATION_ID,
      },
      required: ["project", "name"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "milestone-save",
      ...assigned("--milestone", text(input, "milestone")),
      ...nullable("--target", input, "target", "none"),
      ...nullable("--details", input, "details", ""),
      ...assigned("--status", text(input, "status")),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "project", true),
      text(input, "name", true),
    ],
  },
  {
    name: "sona_milestone_delete",
    title: "Delete a Sona milestone",
    description: `Delete a milestone. Its tasks stay in the project without it. ${CATALOG_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        project: PROJECT,
        milestone: {
          type: "string",
          description: "The milestone, by name or id from sona_milestone_list.",
        },
        operation_id: OPERATION_ID,
      },
      required: ["project", "milestone"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "milestone-delete",
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "project", true),
      text(input, "milestone", true),
    ],
  },
  {
    name: "sona_person_list",
    title: "List people in Sona",
    description:
      "List the people the People page shows, by name: each one's id, the other names they go by, role, organization, emails, notes, open tasks they owe, meeting count and last meeting. query keeps people whose name or any alias contains it. Every tool that takes a person matches the name or any alias.",
    inputSchema: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description: "Only people whose name or alias contains this.",
        },
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "people",
      ...assigned("--query", text(input, "query")),
      "--json",
    ],
  },
  {
    name: "sona_person_save",
    title: "Save a person in Sona",
    description: `Add a person, or change one. Saving a name nobody goes by adds them; saving a name or alias one person goes by changes that person; person renames another person to name and keeps the old name as an alias. A name or alias that already belongs to someone else is refused: merge the two with sona_person_merge. Fields left out stay as they are. ${CATALOG_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The person's name." },
        person: {
          ...PERSON,
          description: `The person to rename to name. ${PERSON.description}`,
        },
        aliases: {
          type: "string",
          description:
            "Every other name they go by, comma-separated. Replaces the list; an empty string or null clears it.",
        },
        role: { type: "string", description: "Their role. An empty string or null clears it." },
        org: {
          type: "string",
          description: "Their organization. An empty string or null clears it.",
        },
        email: {
          type: "string",
          description:
            "Their email addresses, comma-separated. Replaces the list; an empty string or null clears it.",
        },
        notes: { type: "string", description: "Notes about them. An empty string or null clears them." },
        operation_id: OPERATION_ID,
      },
      required: ["name"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-save",
      ...assigned("--person", text(input, "person")),
      ...nullable("--aliases", input, "aliases", ""),
      ...nullable("--role", input, "role", ""),
      ...nullable("--org", input, "org", ""),
      ...nullable("--email", input, "email", ""),
      ...nullable("--notes", input, "notes", ""),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "name", true),
    ],
  },
  {
    name: "sona_person_merge",
    title: "Merge two people in Sona",
    description: `Fold one person into another: their tasks, meeting quotes and speaker links move to into, and person is gone. When both have a voice profile Sona refuses and the user merges them on the People page. ${CATALOG_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        person: { ...PERSON, description: `The person to fold away. ${PERSON.description}` },
        into: { ...PERSON, description: `The person who keeps everything. ${PERSON.description}` },
        operation_id: OPERATION_ID,
      },
      required: ["person", "into"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-merge",
      `--into=${text(input, "into", true)}`,
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
    ],
  },
  {
    name: "sona_meeting_list",
    title: "List Sona meetings with who was in them",
    description:
      "List meetings, newest first: each one's id, title, start, minutes, whether it was processed, and the people in it. Read has_more before treating the list as complete; to see older meetings, raise limit, or narrow with since.",
    inputSchema: {
      type: "object",
      properties: {
        since: {
          type: "string",
          description: "Only meetings from then on: a local day as YYYY-MM-DD, or milliseconds since the epoch.",
        },
        limit: LIMIT,
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "meetings",
      ...assigned("--since", text(input, "since")),
      ...assigned("--limit", count(input, "limit")),
      "--json",
    ],
  },
  {
    name: "sona_meeting_get",
    title: "Read a Sona meeting with its commitments",
    description:
      "Read one meeting: its summary, the commitments it left with who owes them, and with transcript:true every line with who said it.",
    inputSchema: {
      type: "object",
      properties: {
        meeting_id: MEETING_ID,
        transcript: { type: "boolean", description: "Include the transcript with speaker turns." },
      },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "meeting",
      ...(flag(input, "transcript") ? ["--transcript"] : []),
      "--json",
      "--",
      text(input, "meeting_id", true),
    ],
  },
];

/** The tools a server publishes for one `SONA_MCP_TOOLS` value: unset or
 * `core` for every tool not marked opt-in, `all` for every tool, or a
 * comma-separated list of tool names to publish beside the core ones. */
export function publishedTools(selection: string | undefined): readonly ToolDefinition[] {
  const asked = (selection ?? "")
    .split(",")
    .map((name) => name.trim())
    .filter((name) => name !== "" && name !== "core");
  if (asked.includes("all")) return TOOLS;
  const unknown = asked.filter((name) => !TOOLS.some((tool) => tool.name === name));
  if (unknown.length > 0) {
    throw new Error(
      `SONA_MCP_TOOLS names no tool ${unknown.join(", ")}. Use core, all, or tool names: ${TOOLS.filter((tool) => tool.optIn === true)
        .map((tool) => tool.name)
        .join(", ")}.`,
    );
  }
  return TOOLS.filter((tool) => tool.optIn !== true || asked.includes(tool.name));
}
