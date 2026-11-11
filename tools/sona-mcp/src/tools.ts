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
    z.array(
      z.record(
        z.string(),
        z.union([z.string(), z.number(), z.boolean(), z.null(), z.array(z.string())]),
      ),
    ),
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
        "delegate",
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
        "Ids or names; stored statuses (todo, in_progress, ...); priorities 0-4 or urgent, high, medium, low, none; me for the user; agent names (claude, codex, ...) for delegate; none for an empty field; cycle numbers, current or next; days as YYYY-MM-DD, today, tomorrow, yesterday or epoch ms.",
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
  readonly items?: { readonly type: "string" } | typeof CONDITION_SCHEMA | ObjectSchema;
  readonly minItems?: number;
}

/** One list entry Sona reads whole as JSON: a patch step, a plan step, a choice. */
export interface ObjectSchema {
  readonly type: "object";
  readonly properties: Record<
    string,
    { readonly type: string | readonly string[]; readonly description: string; readonly enum?: readonly string[]; readonly items?: { readonly type: "string" } }
  >;
  readonly required: readonly string[];
  readonly additionalProperties: false;
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

function requiredCount(input: ToolInput, key: string): string {
  const value = count(input, key);
  if (value === null) throw new SonaInputError(`${key} is required.`);
  return value;
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

/** A list of objects as the one JSON argument `flag` reads; Sona checks
 * each entry's fields. */
function objects(flagName: string, input: ToolInput, key: string, required: boolean): string[] {
  const value = input[key];
  if (value === undefined || value === null) {
    if (required) throw new SonaInputError(`${key} is required.`);
    return [];
  }
  const parsed = z.array(z.record(z.string(), z.unknown())).safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError(`${key} must be a list of objects.`);
  }
  return [`${flagName}=${JSON.stringify(parsed.data)}`];
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

function joined(flagName: string, input: ToolInput, key: string): string[] {
  const value = input[key];
  if (value === undefined || value === null) return [];
  const parsed = z.array(z.string()).safeParse(value);
  if (!parsed.success) {
    throw new SonaInputError(`${key} must be a list of strings.`);
  }
  return [`${flagName}=${parsed.data.map((entry) => entry.trim()).filter((entry) => entry !== "").join(",")}`];
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
  "This changes the user's Sona projects and people. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. Nothing is proposed for later. dry_run:true shows the result without writing. A call that changes nothing answers with operation_id null.";

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

const SESSION = {
  type: "string",
  description: "An agent session's id, from sona_session_start or sona_session_list.",
} as const;

const PATCH_STEP: ObjectSchema = {
  type: "object",
  properties: {
    field: {
      type: "string",
      enum: ["title", "description", "status", "priority", "owner", "due", "project", "milestone", "labels", "parent", "cycle", "repeat", "snooze"],
      description: "The field this step changes.",
    },
    op: {
      type: "string",
      enum: ["set", "add", "remove", "replace", "append", "prepend", "insert_before", "insert_after", "replace_text"],
      description: "labels: set, add or remove. description: replace, append, prepend, insert_before, insert_after or replace_text. Others take none.",
    },
    value: {
      type: ["string", "integer", "array", "null"],
      items: { type: "string" },
      description: "The new value: text, a name or id, a day (YYYY-MM-DD, today, tomorrow), a status (todo, in_progress, done), a priority; a list for labels; null clears.",
    },
    anchor: { type: "string", description: "For insert_before, insert_after and replace_text: text already in the description." },
  },
  required: ["field"],
  additionalProperties: false,
};

const PLAN_STEP: ObjectSchema = {
  type: "object",
  properties: {
    content: { type: "string", description: "The step." },
    status: { type: "string", enum: ["pending", "in_progress", "completed", "canceled"], description: "Where it stands." },
  },
  required: ["content", "status"],
  additionalProperties: false,
};

const SELECT_OPTION: ObjectSchema = {
  type: "object",
  properties: {
    label: { type: "string", description: "What the user sees." },
    value: { type: "string", description: "What comes back when the user picks it." },
  },
  required: ["label", "value"],
  additionalProperties: false,
};

const PLAN = {
  type: "array",
  items: PLAN_STEP,
  description: "The whole plan, replacing the old one, at most 50 steps.",
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

/** A task write that takes tasks only: delete, subscribe, unsubscribe. */
function onTasks(verb: "delete" | "subscribe" | "unsubscribe", title: string, what: string): ToolDefinition {
  return {
    name: `sona_work_${verb}`,
    title,
    description: `${what} Tasks already that way are listed under skipped. ${ORGANIZE_WRITES}`,
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

/** A read that takes nothing. */
function listing(name: string, verb: string, title: string, description: string): ToolDefinition {
  return {
    name,
    title,
    description,
    optIn: true,
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", verb, "--json"],
  };
}

const VERSION = {
  type: "string",
  description: "A description version's id, from sona_work_description_versions.",
} as const;

const FAVORITE_TARGET = {
  kind: { type: "string", enum: ["task", "project", "view"], description: "What target names." },
  target: {
    type: "string",
    description: "A task's handle (S-12), number or id; a project's name or id; or a saved view's name or id.",
  },
} as const;

/** `favorite` and `unfavorite`: one task, project or saved view. */
function favoriting(verb: "favorite" | "unfavorite", title: string, what: string): ToolDefinition {
  return {
    name: `sona_work_${verb}`,
    title,
    description: `${what} ${ORGANIZE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { ...FAVORITE_TARGET, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["kind", "target"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const kind = text(input, "kind", true);
      const target = text(input, "target", true);
      const named =
        kind === "task" ? ["--", target] : kind === "project" || kind === "view" ? [`--${kind}=${target}`] : null;
      if (named === null) throw new SonaInputError("kind must be task, project or view.");
      return [
        "work",
        verb,
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        ...named,
      ];
    },
  };
}

function lifecycleTools(): ToolDefinition[] {
  return [
    onTasks(
      "delete",
      "Delete Sona tasks to the bin",
      "Move tasks to the bin: hidden from every list, restorable with sona_work_restore for 30 days, then deleted for good.",
    ),
    listing(
      "sona_work_bin",
      "bin",
      "Read Sona's task bin",
      "Read deleted tasks still in the bin, newest first, each with when it was deleted, when it is purged for good and the days left.",
    ),
    listing(
      "sona_work_archive_list",
      "archive-list",
      "Read Sona's archive",
      "Read the archive: archived tasks, and deleted tasks still in the bin with the days left before each is purged.",
    ),
    {
      name: "sona_work_description_versions",
      title: "Read a Sona task's description history",
      description:
        "Read a task's descriptions, newest first: each version's id, its text, who wrote it (you, Sona, an agent's name, or before for the text before any edit), when, and which is current.",
      optIn: true,
      inputSchema: {
        type: "object",
        properties: { task: TASK },
        required: ["task"],
        additionalProperties: false,
      },
      argv: (input) => ["work", "description-versions", "--json", "--", text(input, "task", true)],
    },
    {
      name: "sona_work_description_version",
      title: "Read one Sona description version",
      description: "Read one version of a task's description in full, with its task.",
      optIn: true,
      inputSchema: {
        type: "object",
        properties: { version: VERSION },
        required: ["version"],
        additionalProperties: false,
      },
      argv: (input) => ["work", "description-version", "--json", "--", text(input, "version", true)],
    },
    {
      name: "sona_work_description_restore",
      title: "Restore a Sona task's earlier description",
      description: `Put an earlier description back on its task. ${WRITES} While agents' changes wait for the user it is proposed: proposed:true.`,
      mutates: "changes",
      optIn: true,
      inputSchema: {
        type: "object",
        properties: { version: VERSION, dry_run: DRY_RUN, operation_id: OPERATION_ID },
        required: ["version"],
        additionalProperties: false,
      },
      argv: (input, agent) => [
        "work",
        "description-restore",
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "version", true),
      ],
    },
    favoriting("favorite", "Favorite a Sona task, project or view", "Add a task, a project or a saved view to the user's favorites."),
    favoriting("unfavorite", "Unfavorite a Sona task, project or view", "Take a task, a project or a saved view out of the user's favorites."),
    listing(
      "sona_work_favorites",
      "favorites",
      "Read Sona's favorites",
      "Read the user's favorite tasks, projects and saved views, in the order added.",
    ),
    onTasks(
      "subscribe",
      "Subscribe to Sona tasks",
      "Subscribe the user to tasks: status changes, comments, mentions and assignments others make on them reach the inbox.",
    ),
    onTasks("unsubscribe", "Unsubscribe from Sona tasks", "Unsubscribe the user from tasks."),
    listing(
      "sona_work_subscriptions",
      "subscriptions",
      "Read Sona task subscriptions",
      "Read the tasks the user is subscribed to and why: subscribed, created, assigned or mentioned. Creating a task, being made its owner or being mentioned in a comment subscribes the user.",
    ),
    {
      name: "sona_work_inbox",
      title: "Read Sona's inbox",
      description:
        "Read the inbox: status changes, comments, mentions and assignments that Sona or agents made on tasks the user is subscribed to, newest first, with the unread count. The user's own changes never notify. Snoozed ones stay out until they wake, archived ones until asked for.",
      optIn: true,
      inputSchema: {
        type: "object",
        properties: {
          unread: { type: "boolean", description: "Only unread notifications." },
          snoozed: { type: "boolean", description: "Also snoozed ones that have not woken yet." },
          archived: { type: "boolean", description: "Also archived ones." },
        },
        additionalProperties: false,
      },
      argv: (input) => [
        "work",
        "inbox",
        ...(flag(input, "unread") ? ["--unread"] : []),
        ...(flag(input, "snoozed") ? ["--snoozed"] : []),
        ...(flag(input, "archived") ? ["--archived"] : []),
        "--json",
      ],
    },
    {
      name: "sona_work_inbox_mark",
      title: "Mark Sona inbox notifications",
      description: `Mark notifications read or unread, archive or unarchive them, or snooze them until a day. notifications "all" with mark read marks every unread one. ${ORGANIZE_WRITES}`,
      mutates: "changes",
      optIn: true,
      inputSchema: {
        type: "object",
        properties: {
          notifications: {
            type: "string",
            description: 'Notification ids from sona_work_inbox, comma-separated, or "all" for every unread one.',
          },
          mark: {
            type: "string",
            enum: ["read", "unread", "archive", "unarchive", "snooze"],
            description: "What to do to them. snooze hides them until the until day.",
          },
          until: { type: "string", description: "For snooze: YYYY-MM-DD, tomorrow, or none to wake it now." },
          dry_run: DRY_RUN,
          operation_id: OPERATION_ID,
        },
        required: ["notifications", "mark"],
        additionalProperties: false,
      },
      argv: (input, agent) => {
        const every = text(input, "notifications", true).trim().toLowerCase() === "all";
        return [
          "work",
          "inbox-mark",
          `--mark=${text(input, "mark", true)}`,
          ...assigned("--until", text(input, "until")),
          ...(every ? ["--all"] : []),
          ...(flag(input, "dry_run") ? ["--dry-run"] : []),
          ...writing(input, agent),
          "--json",
          ...(every ? [] : ["--", ...tasks(input, "notifications")]),
        ];
      },
    },
  ];
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
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task", "link", "other"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      verb,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
      text(input, "link", true),
      text(input, "other", true),
    ],
  };
}

const DAY = {
  type: "string",
  description: "A local day: YYYY-MM-DD, today or tomorrow. Today unless told.",
} as const;

const LOOP_ID = {
  type: "string",
  description:
    "The loop's own address, as it appears in a sona://loop/<id> link or in the id field of a sona_action_items row.",
} as const;

const NOTE_ID = {
  type: "string",
  description: "A note's id, as sona_search with scope notes returns it.",
} as const;

const ORGANIZE_WRITES =
  "This changes the user's Sona tasks. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. Nothing is proposed for later. dry_run:true shows the result without writing. Repeating an operation_id answers replayed:true.";

const LOOP_WRITES = `${ORGANIZE_WRITES} The meeting's history names the user as the one who changed the loop.`;

const PEOPLE_WRITES =
  "This changes the user's Sona people. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. Meetings kept on this Mac answer not_found, and a person in one is left alone. dry_run:true shows the result without writing. Repeating an operation_id answers replayed:true.";

const NOTE_WRITES =
  "This changes the user's Sona notes, nothing else. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. dry_run:true shows the result without writing.";

const DOCUMENT_WRITES =
  "This changes the user's Sona documents, nothing else. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. dry_run:true shows the result without writing.";

const DOCUMENT_OPERATION =
  "A UUID naming this change so a retry cannot apply it twice: the same id answers the first result with replayed:true. Left out, each call is new.";

const NOTE_OPERATION =
  "A UUID naming this change so a retry cannot apply it twice: the same id with the same request answers the first note with replayed:true. Left out, each call is new.";

const AGENT_LOOP_WRITES =
  "This changes the user's saved automations in Sona, nothing else. It needs Settings > Agents > External access and External mutations; with either off Sona refuses with consent_required and names the setting. dry_run:true shows the result without writing.";

const AGENT_LOOP_KIND =
  "An automation is a saved agent workflow: when its trigger fires, the running Sona app has its in-app agent carry out the automation's prompt, using only the read-only tools on the automation's list. It is not a meeting commitment loop (sona_loop_resolve, sona_meeting_loop_*).";

const AGENT_LOOP = {
  type: "string",
  description: "An automation's name or id, from sona_automation_list.",
} as const;

function onAgentLoop(
  verb: "run" | "enable" | "disable" | "delete",
  title: string,
  what: string,
  access: { readonly mutates: "changes" | "destroys"; readonly optIn?: true },
): ToolDefinition {
  return {
    name: `sona_automation_${verb}`,
    title,
    description: `${what} ${AGENT_LOOP_KIND} ${AGENT_LOOP_WRITES}`,
    ...access,
    inputSchema: {
      type: "object",
      properties: { automation: AGENT_LOOP, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["automation"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      `automation-${verb}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "automation", true),
    ],
  };
}

function actionAside(verb: "dismiss" | "restore", title: string, what: string): ToolDefinition {
  return {
    name: `sona_planning_${verb}_action`,
    title,
    description: `${what} ${ORGANIZE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { loop_id: LOOP_ID, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["loop_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      `action-${verb}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "loop_id", true),
    ],
  };
}

const SETTING_KEYS = [
  "push-to-talk",
  "audio-feedback",
  "audio-feedback-volume",
  "sound-theme",
  "mute-while-recording",
  "quiet-speech",
  "command-mode",
  "overlay",
  "dictation-languages",
  "app-language",
  "theme",
  "log-level",
  "debug-mode",
  "cleanup",
  "cleanup-provider",
  "cleanup-model",
  "cleanup-base-url",
  "evening-digest",
  "snippets",
  "text-replacements",
  "mode",
  "dictation-prompt",
  "keyword-trackers",
  "meeting-prep",
] as const;

const SETTINGS_WRITES =
  "Needs Sona's outside changes allowed, and obeys its Apply or Suggest setting. A running Sona follows the change within a second, as if it were made in Settings.";

const SETTINGS_OPERATION_ID = {
  type: "string",
  description:
    "A UUID naming this change so a retry cannot apply it twice. Left out, each call is a new change.",
} as const;

const MODE_ID = { type: "string", description: "A mode's id, from sona_settings under modes." } as const;

const SETTINGS_STORE_WRITES =
  "Needs Sona's outside changes allowed, and obeys its Apply or Suggest setting. A meeting kept on this Mac answers not_found.";

/** What every settings write ends with before its own arguments. */
function settingsWrite(verb: string, input: ToolInput, agent: string | undefined): string[] {
  return [
    "work",
    verb,
    ...(flag(input, "dry_run") ? ["--dry-run"] : []),
    ...assigned("--operation-id", text(input, "operation_id")),
    ...assigned("--agent", agent ?? null),
    "--json",
  ];
}

const APPLIED_WRITES =
  "It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. dry_run:true shows the result without writing. Repeating an operation_id, or asking for what is already so, answers replayed:true.";
const LIBRARY_WRITES = `A meeting kept on this Mac answers not_found. ${APPLIED_WRITES}`;

const FOLDER = { type: "string", description: "The folder, by name or by id from sona_meeting_folders." } as const;
const SPEAKER = { type: "string", description: "The speaker as the transcript names them (\"Speaker 2\"), or their speaker_id from sona_meeting_get." } as const;
const LINE_ID = { type: "string", description: "The line's segment_id from sona_transcript." } as const;
const CORRECTION_ID = { type: "string", description: "The correction's id from sona_transcript_corrections." } as const;
const DICTATION_ID = { type: "integer", description: "The dictation's id from sona_dictations." } as const;
const CORRECTION_FIELDS = {
  kind: { type: "string", enum: ["phrase", "name", "term", "dialect", "code-switching"], description: "What sort of correction it is. phrase unless set." },
  context: { type: "string", enum: ["person", "conversation"], description: "person: wherever this person says it. conversation: only in the meeting the line is from. Dialect and code-switching need conversation or around." },
  around: { type: "string", description: "Apply it only where the line also says this phrase. Not with context." },
} as const;

function correctionFlags(input: ToolInput): string[] {
  return [
    ...assigned("--heard", text(input, "heard")),
    ...assigned("--fix", text(input, "fix")),
    ...assigned("--kind", text(input, "kind")),
    ...assigned("--context", text(input, "context")),
    ...assigned("--around", text(input, "around")),
  ];
}

function dictationId(input: ToolInput): string {
  const id = count(input, "dictation_id");
  if (id === null) throw new SonaInputError("dictation_id is required.");
  return id;
}
const MEETING_NOTE_ID = { type: "string", description: "The note's id from sona_meeting_notes." } as const;
const SNAPSHOT_ID = { type: "string", description: "The snapshot's id from sona_meeting_snapshots." } as const;
const PLAN_ID = { type: "string", description: "The plan's id from sona_meeting_heard_plans." } as const;
const OUT = { type: "string", description: "Absolute path of the file to write; Sona replaces it whole." } as const;
const TRASH_ID = { type: "string", description: "The trash entry's id from sona_meeting_trash, or the trash_id sona_meeting_delete answered." } as const;

/** A meeting write: the meeting first, then what the verb names after it. */
function meetingWrite(
  name: string,
  title: string,
  description: string,
  mutates: "changes" | "destroys",
  verb: string,
  properties: Record<string, ToolProperty>,
  required: readonly string[],
  flags: (input: ToolInput) => string[],
  positional: (input: ToolInput) => string[],
  rules = LIBRARY_WRITES,
): ToolDefinition {
  return {
    name,
    title,
    description: `${description} ${rules}`,
    mutates,
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { ...properties, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required,
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      verb,
      ...flags(input),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      ...positional(input),
    ],
  };
}

/** A meeting read: plain flags, then the values the verb names. */
function meetingRead(
  name: string,
  title: string,
  description: string,
  verb: string,
  properties: Record<string, ToolProperty>,
  required: readonly string[],
  flags: (input: ToolInput) => string[],
  positional: (input: ToolInput) => string[],
): ToolDefinition {
  return {
    name,
    title,
    description,
    inputSchema: { type: "object", properties, required, additionalProperties: false },
    argv: (input) => ["work", verb, ...flags(input), "--json", "--", ...positional(input)],
  };
}

const none = (): string[] => [];
const meetingOnly = (input: ToolInput): string[] => [text(input, "meeting_id", true)];

function meetingLibraryTools(): ToolDefinition[] {
  return [
    meetingRead(
      "sona_meeting_folders",
      "List Sona's meeting folders",
      "List the folders meetings are filed in, as the meetings list's folder chips show them: each one's id, name and how many meetings it holds.",
      "folders",
      {},
      [],
      none,
      none,
    ),
    meetingRead(
      "sona_meeting_notes",
      "Read a Sona meeting's notes",
      "Read one meeting's notes: the notes Sona wrote, the user's own notes, the notes typed against moments in the recording with their ids, and the action items numbered as sona_meeting_action_done takes them, each with whether it is done.",
      "meeting-notes",
      { meeting_id: MEETING_ID },
      ["meeting_id"],
      none,
      meetingOnly,
    ),
    meetingRead(
      "sona_meeting_snapshots",
      "List a Sona meeting's screen snapshots",
      "List the screen snapshots kept during one meeting, oldest first, with each one's id, the moment it was taken and the text read from it.",
      "snapshots",
      { meeting_id: MEETING_ID },
      ["meeting_id"],
      none,
      meetingOnly,
    ),
    meetingRead(
      "sona_meeting_heard_plans",
      "List the plans Sona heard in a meeting",
      "List the plans Sona heard someone make in one meeting, with each plan's id, title, time, people, place, and whether it is already on the calendar or was set aside.",
      "heard-plans",
      { meeting_id: MEETING_ID },
      ["meeting_id"],
      none,
      meetingOnly,
    ),
    meetingRead(
      "sona_said_search",
      "Search what the user said in meetings",
      "Find the lines the user said across meetings that contain query, newest first, each with its meeting and moment.",
      "said",
      { query: { type: "string", description: "Words the line contains." }, limit: LIMIT },
      ["query"],
      (input) => assigned("--limit", count(input, "limit")),
      (input) => [text(input, "query", true)],
    ),
    meetingRead(
      "sona_screen_text_search",
      "Search text Sona read off the screen in meetings",
      "Find the screen snapshots whose text contains query, newest first, each with its meeting and moment.",
      "screen-text",
      { query: { type: "string", description: "Words the screen text contains." }, limit: LIMIT },
      ["query"],
      (input) => assigned("--limit", count(input, "limit")),
      (input) => [text(input, "query", true)],
    ),
    meetingRead(
      "sona_meeting_search",
      "Search inside one Sona meeting",
      "Find where query comes up in one meeting, as the search box in an open meeting does: transcript lines, notes and screen text, each with its moment.",
      "meeting-search",
      { meeting_id: MEETING_ID, query: { type: "string", description: "Words to find." }, limit: LIMIT },
      ["meeting_id", "query"],
      (input) => assigned("--limit", count(input, "limit")),
      (input) => [text(input, "meeting_id", true), text(input, "query", true)],
    ),
    meetingRead(
      "sona_meeting_ledger",
      "Read a Sona meeting's ledger page",
      "The meeting's ledger as one HTML page, the page the app saves.",
      "meeting-ledger",
      { meeting_id: MEETING_ID },
      ["meeting_id"],
      none,
      meetingOnly,
    ),
    meetingRead(
      "sona_meetings_csv",
      "Every Sona meeting as CSV",
      "Every meeting as one CSV, as the app's Export all does, leaving out meetings kept on this Mac.",
      "meetings-csv",
      {},
      [],
      none,
      none,
    ),
    meetingWrite(
      "sona_meeting_export",
      "Export a Sona meeting to a file",
      "Write one meeting's notes and transcript to out as Markdown or JSON, with its pictures beside the file, as the app's Export does.",
      "changes",
      "meeting-export",
      { meeting_id: MEETING_ID, format: { type: "string", enum: ["markdown", "json"], description: "Markdown unless json." }, out: OUT },
      ["meeting_id", "out"],
      (input) => [...assigned("--format", text(input, "format")), ...assigned("--out", text(input, "out", true))],
      meetingOnly,
    ),
    meetingWrite(
      "sona_meeting_rename",
      "Rename a Sona meeting",
      "Give one meeting a new title.",
      "changes",
      "meeting-rename",
      { meeting_id: MEETING_ID, title: { type: "string", description: "The new title." } },
      ["meeting_id", "title"],
      none,
      (input) => [text(input, "meeting_id", true), text(input, "title", true)],
    ),
    meetingWrite(
      "sona_meeting_delete",
      "Move a Sona meeting to the trash",
      "Move one meeting to the trash, where Sona keeps it 30 days. The answer's trash_id brings it back with sona_meeting_restore.",
      "destroys",
      "meeting-delete",
      { meeting_id: MEETING_ID },
      ["meeting_id"],
      none,
      meetingOnly,
    ),
    meetingRead(
      "sona_meeting_trash",
      "List Sona's meeting trash",
      "List the meetings in the trash, as the app's Trash shows them: each entry's id, title, when it was deleted, and when Sona deletes it for good (null for one a retention setting removed). Meetings kept on this Mac are left out.",
      "meeting-trash",
      {},
      [],
      none,
      none,
    ),
    meetingWrite(
      "sona_meeting_restore",
      "Restore a Sona meeting from the trash",
      "Put one trashed meeting back where it was, filed in its folders again. The answer names the meeting_id.",
      "changes",
      "meeting-restore",
      { trash_id: TRASH_ID },
      ["trash_id"],
      none,
      (input) => [text(input, "trash_id", true)],
    ),
    meetingWrite(
      "sona_meeting_delete_forever",
      "Delete a trashed Sona meeting for good",
      "Delete one trashed meeting and its audio for good, before its 30 days are up. Nothing brings it back.",
      "destroys",
      "meeting-delete-forever",
      { trash_id: TRASH_ID },
      ["trash_id"],
      none,
      (input) => [text(input, "trash_id", true)],
    ),
    meetingWrite(
      "sona_meeting_folder_new",
      "Make a Sona meeting folder",
      "Make a folder to file meetings in.",
      "changes",
      "folder-new",
      { name: { type: "string", description: "The folder's name." } },
      ["name"],
      none,
      (input) => [text(input, "name", true)],
    ),
    meetingWrite(
      "sona_meeting_folder_rename",
      "Rename a Sona meeting folder",
      "Give a folder a new name.",
      "changes",
      "folder-rename",
      { folder: FOLDER, name: { type: "string", description: "The new name." } },
      ["folder", "name"],
      none,
      (input) => [text(input, "folder", true), text(input, "name", true)],
    ),
    meetingWrite(
      "sona_meeting_folder_delete",
      "Delete a Sona meeting folder",
      "Delete a folder. The meetings in it stay; only the folder and their filing in it are gone, which no tool brings back.",
      "destroys",
      "folder-delete",
      { folder: FOLDER },
      ["folder"],
      none,
      (input) => [text(input, "folder", true)],
    ),
    ...(["add", "remove"] as const).map((way) =>
      meetingWrite(
        way === "add" ? "sona_meeting_file" : "sona_meeting_unfile",
        way === "add" ? "File a Sona meeting in a folder" : "Take a Sona meeting out of a folder",
        way === "add" ? "File one meeting in a folder." : "Take one meeting out of a folder; the meeting stays.",
        "changes",
        `folder-${way}`,
        { meeting_id: MEETING_ID, folder: FOLDER },
        ["meeting_id", "folder"],
        (input) => assigned("--folder", text(input, "folder", true)),
        meetingOnly,
      ),
    ),
    meetingWrite(
      "sona_meeting_note_add",
      "Add a note to a Sona meeting",
      "Add a note to one meeting, pinned to a moment in the recording when at_seconds is given.",
      "changes",
      "meeting-note-add",
      { meeting_id: MEETING_ID, text: { type: "string", description: "The note." }, at_seconds: { type: "integer", minimum: 0, description: "Seconds into the recording the note is about." } },
      ["meeting_id", "text"],
      (input) => assigned("--at", count(input, "at_seconds")),
      (input) => [text(input, "meeting_id", true), text(input, "text", true)],
    ),
    meetingWrite(
      "sona_meeting_note_edit",
      "Change a note on a Sona meeting",
      "Replace the text of one note on a meeting.",
      "changes",
      "meeting-note-edit",
      { meeting_id: MEETING_ID, note_id: MEETING_NOTE_ID, text: { type: "string", description: "The note's new text." } },
      ["meeting_id", "note_id", "text"],
      none,
      (input) => [text(input, "meeting_id", true), text(input, "note_id", true), text(input, "text", true)],
    ),
    meetingWrite(
      "sona_meeting_note_delete",
      "Delete a note from a Sona meeting",
      "Delete one note from a meeting. No tool brings it back.",
      "destroys",
      "meeting-note-delete",
      { meeting_id: MEETING_ID, note_id: MEETING_NOTE_ID },
      ["meeting_id", "note_id"],
      none,
      (input) => [text(input, "meeting_id", true), text(input, "note_id", true)],
    ),
    meetingWrite(
      "sona_meeting_my_notes",
      "Replace the user's own notes on a Sona meeting",
      "Replace the user's own notes on one meeting with text. What was there before is gone; read it with sona_meeting_notes first and keep what should stay.",
      "destroys",
      "my-notes",
      { meeting_id: MEETING_ID, text: { type: "string", description: "The notes, whole." } },
      ["meeting_id", "text"],
      none,
      (input) => [text(input, "meeting_id", true), text(input, "text", true)],
    ),
    meetingWrite(
      "sona_meeting_action_done",
      "Tick an action item in a Sona meeting",
      "Mark one of a meeting's action items done, or with undo:true not done. number is the item's number from sona_meeting_notes.",
      "changes",
      "action-done",
      { meeting_id: MEETING_ID, number: { type: "integer", minimum: 1, description: "The action item's number." }, undo: { type: "boolean", description: "Mark it not done." } },
      ["meeting_id", "number"],
      (input) => (flag(input, "undo") ? ["--undo"] : []),
      (input) => [text(input, "meeting_id", true), requiredCount(input, "number")],
    ),
    meetingWrite(
      "sona_meeting_snapshot_delete",
      "Delete a screen snapshot from a Sona meeting",
      "Delete one screen snapshot and the text read from it. No tool brings it back.",
      "destroys",
      "snapshot-delete",
      { meeting_id: MEETING_ID, snapshot_id: SNAPSHOT_ID },
      ["meeting_id", "snapshot_id"],
      none,
      (input) => [text(input, "meeting_id", true), text(input, "snapshot_id", true)],
    ),
    meetingWrite(
      "sona_heard_plan_edit",
      "Correct a plan Sona heard in a meeting",
      "Correct a heard plan's title, time, people or place before the user adds it to the calendar. Fields left out stay as they are; an empty place clears it. A plan already on the calendar answers conflict. start and end take milliseconds since the epoch, RFC 3339, or a local YYYY-MM-DD HH:MM.",
      "changes",
      "heard-plan-edit",
      {
        plan_id: PLAN_ID,
        title: { type: "string", description: "The plan's title." },
        start: { type: "string", description: "When it starts." },
        end: { type: "string", description: "When it ends." },
        people: { type: "array", items: { type: "string" }, description: "Who it is with, every name; an empty list clears them." },
        place: { type: "string", description: "Where it is." },
      },
      ["plan_id"],
      (input) => [
        ...assigned("--title", text(input, "title")),
        ...assigned("--start", text(input, "start")),
        ...assigned("--end", text(input, "end")),
        ...(input.people === undefined || input.people === null ? [] : [`--people=${people(input)}`]),
        ...cleared("--place", input, "place"),
      ],
      (input) => [text(input, "plan_id", true)],
    ),
    meetingWrite(
      "sona_heard_plan_not_now",
      "Set aside a plan Sona heard in a meeting",
      "Set a heard plan aside, as Not now does, so Sona stops offering it.",
      "changes",
      "heard-plan-not-now",
      { plan_id: PLAN_ID },
      ["plan_id"],
      none,
      (input) => [text(input, "plan_id", true)],
    ),
    meetingWrite(
      "sona_speaker_rename",
      "Rename a speaker in a Sona meeting",
      "Change the name one speaker goes by in a meeting's transcript, as the transcript's speaker menu does. Their voice samples stay.",
      "changes",
      "speaker-rename",
      { meeting_id: MEETING_ID, speaker: SPEAKER, name: { type: "string", description: "The name the transcript shows from now on." } },
      ["meeting_id", "speaker", "name"],
      none,
      (input) => [text(input, "meeting_id", true), text(input, "speaker", true), text(input, "name", true)],
    ),
    meetingWrite(
      "sona_speaker_merge",
      "Merge two speakers in a Sona meeting",
      "Move every line of speaker into into, as the transcript's Merge does. Voice samples kept from speaker are deleted and the merge cannot be undone here.",
      "destroys",
      "speaker-merge",
      { meeting_id: MEETING_ID, speaker: SPEAKER, into: SPEAKER },
      ["meeting_id", "speaker", "into"],
      (input) => assigned("--into", text(input, "into", true)),
      (input) => [text(input, "meeting_id", true), text(input, "speaker", true)],
    ),
    meetingWrite(
      "sona_transcript_line_edit",
      "Change a line of a Sona meeting's transcript",
      "Replace the words of one transcript line, as editing a line in the transcript does. The words the transcriber heard stay in Sona's history.",
      "changes",
      "line-edit",
      { meeting_id: MEETING_ID, line_id: LINE_ID, text: { type: "string", description: "The words the line should read." } },
      ["meeting_id", "line_id", "text"],
      none,
      (input) => [text(input, "meeting_id", true), text(input, "line_id", true), text(input, "text", true)],
    ),
    meetingRead(
      "sona_transcript_corrections",
      "List a person's transcript corrections",
      "List the corrections Sona applies to how one person's words are heard, as Transcript corrections on their page shows: each one's id, the words heard and said, its kind, where it applies and whether it is paused. Corrections taught in meetings kept on this Mac are left out.",
      "corrections",
      { person: PERSON },
      ["person"],
      none,
      (input) => [text(input, "person", true)],
    ),
    meetingWrite(
      "sona_transcript_correction_add",
      "Teach Sona a transcript correction",
      "Teach a correction from one transcript line, as Fix how this was heard does: heard is the words the transcriber wrote in the line, fix the words the person said. The line's speaker must already be a confirmed person; the correction is theirs.",
      "changes",
      "correction-add",
      {
        meeting_id: MEETING_ID,
        line_id: LINE_ID,
        heard: { type: "string", description: "The words as they appear in the line." },
        fix: { type: "string", description: "The words the person said." },
        ...CORRECTION_FIELDS,
      },
      ["meeting_id", "line_id", "heard", "fix"],
      correctionFlags,
      (input) => [text(input, "meeting_id", true), text(input, "line_id", true)],
    ),
    meetingWrite(
      "sona_transcript_correction_edit",
      "Change a transcript correction",
      "Change, pause or resume one of a person's transcript corrections. Fields left out keep their value; enabled:false pauses it and enabled:true resumes it.",
      "changes",
      "correction-edit",
      {
        person: PERSON,
        correction_id: CORRECTION_ID,
        heard: { type: "string", description: "The words heard." },
        fix: { type: "string", description: "The words said." },
        ...CORRECTION_FIELDS,
        enabled: { type: "boolean", description: "false pauses it; true resumes it." },
      },
      ["person", "correction_id"],
      (input) => {
        const unset = input.enabled === undefined || input.enabled === null;
        const toggled = unset ? [] : [flag(input, "enabled") ? "--resume" : "--pause"];
        return [...correctionFlags(input), ...toggled];
      },
      (input) => [text(input, "person", true), text(input, "correction_id", true)],
    ),
    meetingWrite(
      "sona_transcript_correction_remove",
      "Delete a transcript correction",
      "Delete one of a person's transcript corrections. Lines it changed read as heard again.",
      "destroys",
      "correction-remove",
      { person: PERSON, correction_id: CORRECTION_ID },
      ["person", "correction_id"],
      none,
      (input) => [text(input, "person", true), text(input, "correction_id", true)],
    ),
    meetingRead(
      "sona_dictations",
      "List the user's Sona dictations",
      "List dictation history newest first, as the Library shows it: each entry's id, time, title, the finished words and the words heard. search narrows it to entries whose words match. next_before, when present, is the before for the next page. It needs Settings > Agents > External access, the same grant sona_search needs for dictations.",
      "dictations",
      {
        search: { type: "string", description: "Words the dictation contains; at least two characters." },
        limit: { ...LIMIT, description: "How many entries to return. Sona defaults to 20 and caps at 100." },
        before: { type: "integer", description: "Only entries older than this id: the next_before of the previous page." },
      },
      [],
      (input) => [
        ...assigned("--search", text(input, "search")),
        ...assigned("--limit", count(input, "limit")),
        ...assigned("--before", count(input, "before")),
      ],
      none,
    ),
    meetingWrite(
      "sona_dictation_save",
      "Keep or unkeep a Sona dictation",
      "Keep a dictation entry, as the Library's save button does, or stop keeping it with saved:false.",
      "changes",
      "dictation-save",
      { dictation_id: DICTATION_ID, saved: { type: "boolean", description: "false stops keeping it. true unless set." } },
      ["dictation_id"],
      (input) => (input.saved === undefined || input.saved === null || flag(input, "saved") ? [] : ["--unsave"]),
      (input) => [dictationId(input)],
      APPLIED_WRITES,
    ),
    meetingWrite(
      "sona_dictation_delete",
      "Delete a Sona dictation",
      "Delete a dictation entry and its recording, as the Library's delete does. It cannot be undone.",
      "destroys",
      "dictation-delete",
      { dictation_id: DICTATION_ID },
      ["dictation_id"],
      none,
      (input) => [dictationId(input)],
      APPLIED_WRITES,
    ),
  ];
}

function people(input: ToolInput): string {
  const parsed = z.array(z.string()).safeParse(input.people);
  if (!parsed.success) throw new SonaInputError("people must be a list of strings.");
  return parsed.data.map((name) => name.trim()).filter((name) => name !== "").join(",");
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
        source: {
          type: "string",
          description:
            "Only work that came from these, comma-separated: meeting, dictation, agent, person. Not with ready.",
        },
        stale: {
          type: "boolean",
          description: "Only open work nobody has touched for 30 days. Not with ready.",
        },
        delegate: {
          type: "string",
          description:
            "Only work delegated to this agent, by the name it gave; any for every delegated task, none for work no agent has. Not with ready.",
        },
        filter: FILTER,
        where: {
          type: "string",
          description:
            'The filter in plain words, such as "high priority bugs due this week that are mine". The answer\'s read_as gives each condition the words were read as (the same shape filter takes) and not_understood the words that did not narrow the list; check it before trusting the list. Not with ready.',
        },
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
      ...(flag(input, "ready") ? [] : assigned("--source", text(input, "source"))),
      ...(flag(input, "ready") || !flag(input, "stale") ? [] : ["--stale"]),
      ...(flag(input, "ready") ? [] : assigned("--delegate", text(input, "delegate"))),
      ...conditions(input),
      ...(flag(input, "ready") ? [] : assigned("--where", text(input, "where"))),
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
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["title"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "create",
      ...fieldFlags(input),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
            "Hide the task until this local day: YYYY-MM-DD, today, tomorrow, later-today (the next hour three hours from now, refused once that is past midnight), or none to wake it.",
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
    description: `Decide on work Sona pulled out of meetings. accept moves triage items to Todo with Sona's suggested owner, due day and project unless ignore_suggestions is true. decline cancels them and sets their meeting commitments aside; reason is kept in each task's history. duplicate marks one task a duplicate of another (of), moving its quotes and links there. Items not in triage are left alone and listed under skipped. dry_run:true previews accept and decline. ${WRITES}`,
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
        reason: {
          type: "string",
          description: "For decline: why, kept in each task's history.",
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
            ...assigned("--reason", text(input, "reason")),
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
  archiving("restore", "Restore Sona tasks", "Bring archived tasks back into their lists, or deleted tasks out of the bin as they were. One call restores from one place."),
  ...lifecycleTools(),
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
        dry_run: DRY_RUN,
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
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["label"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "label-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "label", true),
    ],
  },
  {
    name: "sona_work_label_group",
    title: "Group a Sona label",
    description: `Put a label in a label group, or take it out of its group with group none. A task keeps one label per group: adding another label from the group replaces the one it had. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        label: { type: "string", description: "The label's name or id." },
        group: {
          type: "string",
          description: "The group's name, or none to take the label out of its group.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["label", "group"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "label-group",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "label", true),
      text(input, "group") ?? "none",
    ],
  },
  {
    name: "sona_work_label_retire",
    title: "Retire a Sona label",
    description: `Retire a label: tasks that have it keep it, and pickers and new tags leave it out. restore:true brings it back. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        label: { type: "string", description: "The label's name or id." },
        restore: { type: "boolean", description: "Bring a retired label back." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["label"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "label-retire",
      ...(flag(input, "restore") ? ["--restore"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "label", true),
    ],
  },
  {
    name: "sona_work_templates",
    title: "List Sona task templates",
    description:
      "List task templates: each one's name, id, the title, details, priority, owner, project, labels, due offset in days and repeat a task made from it takes, its sub-task titles, and revision.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "templates", "--json"],
  },
  {
    name: "sona_work_template_save",
    title: "Save a Sona task template",
    description: `Create a task template, or change one: saving a name that exists changes that template; template renames another to name. Fields left out keep what the template had. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The template's name." },
        template: {
          type: "string",
          description: "The template to rename to name, by name or id.",
        },
        title: {
          type: "string",
          description: "The task's title. Left out, the template's name.",
        },
        details: FIELDS.details,
        priority: FIELDS.priority,
        owner: FIELDS.owner,
        project: FIELDS.project,
        labels: FIELDS.labels,
        due_in_days: {
          type: "string",
          description: "Due this many days after the day the task is made, 0 to 3650; none clears it.",
        },
        repeat: FIELDS.repeat,
        subtasks: {
          type: "array",
          items: { type: "string" },
          description: "Sub-task titles, replacing the template's; an empty list clears them.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["name"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const subtasks = input.subtasks === undefined || input.subtasks === null
        ? null
        : z.array(z.string()).safeParse(input.subtasks);
      if (subtasks !== null && !subtasks.success) {
        throw new SonaInputError("subtasks must be a list of strings.");
      }
      const titles = subtasks === null ? [] : subtasks.data.filter((title) => title.trim() !== "");
      return [
        "work",
        "template-save",
        ...assigned("--template", text(input, "template")),
        ...cleared("--title", input, "title"),
        ...cleared("--details", input, "details"),
        ...assigned("--priority", text(input, "priority")),
        ...assigned("--owner", text(input, "owner")),
        ...assigned("--project", text(input, "project")),
        ...cleared("--labels", input, "labels"),
        ...assigned("--due-in", text(input, "due_in_days")),
        ...assigned("--repeat", text(input, "repeat")),
        ...(subtasks === null ? [] : titles.length === 0 ? ["--subtask="] : titles.map((title) => `--subtask=${title}`)),
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "name", true),
      ];
    },
  },
  {
    name: "sona_work_template_delete",
    title: "Delete a Sona task template",
    description: `Delete a task template. Tasks made from it stay. ${WRITES}`,
    mutates: "destroys",
    inputSchema: {
      type: "object",
      properties: {
        template: { type: "string", description: "The template's name or id." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["template"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "template-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "template", true),
    ],
  },
  {
    name: "sona_work_template_use",
    title: "Make a Sona task from a template",
    description: `Make a task from a template with its fields, then its sub-tasks under it, which take the task's project, priority and owner. Retired or deleted labels and a project put away are left off. The same call on the same day makes the task once. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        template: { type: "string", description: "The template's name or id." },
        title: { type: "string", description: "The task's title in place of the template's." },
        due: {
          type: "string",
          description: "The local day it is due in place of the template's: YYYY-MM-DD, today or tomorrow.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["template"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "template-use",
      ...assigned("--title", text(input, "title")),
      ...assigned("--due", text(input, "due")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "template", true),
    ],
  },
  {
    name: "sona_work_remind",
    title: "Set a reminder on a Sona task",
    description: `Set when Sona reminds the user of a task, replacing any reminder it had, or clear it with when none. ${WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        when: {
          type: "string",
          description:
            "A local time: 2026-11-12 09:00, tomorrow 9am, friday 3pm; a day alone means 17:00. none clears the reminder.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task", "when"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "remind",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
      text(input, "when", true),
    ],
  },
  {
    name: "sona_work_reminders",
    title: "List Sona reminders",
    description:
      "List reminders on tasks, soonest first, each with its task, time, who set it and whether Sona has shown it. due:true lists only those whose time has come and that Sona has not shown yet. Reminders on work kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: {
        due: { type: "boolean", description: "Only reminders whose time has come." },
      },
      additionalProperties: false,
    },
    argv: (input) => ["work", "reminders", ...(flag(input, "due") ? ["--due"] : []), "--json"],
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
        show_empty_groups: { type: "boolean", description: "Show groups that hold no tasks." },
        properties: {
          type: "array",
          items: { type: "string" },
          description:
            "The properties each row shows, in order: id, priority, labels, project, owner, due, meeting or updated.",
        },
        filter: FILTER,
        dry_run: DRY_RUN,
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
      ...(input.show_empty_groups === undefined
        ? []
        : [`--show-empty-groups=${flag(input, "show_empty_groups")}`]),
      ...(input.properties === undefined
        ? []
        : [`--properties=${strings(input, "properties").join(",")}`]),
      ...conditions(input),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["view"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "view-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
          description: `Who leads it: me (the user), or a person. ${PERSON.description} nobody, none or null clears it.`,
        },
        members: {
          type: "string",
          description:
            "Every member, comma-separated, each as me (the user) or a name, alias or id from sona_person_list. Replaces the list; an empty string or null clears it.",
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
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["project"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "project-archive",
      ...(flag(input, "restore") ? ["--restore"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
    name: "sona_project_timeline",
    title: "Show Sona projects on a timeline",
    description:
      "List the projects whose days fall between from and to, earliest first, each with its status, start and target day, and the milestones with a target day in that span (id, name, status, target day, open and done task counts, revision). undated names the projects with neither a start nor a target day. Change a project's days with sona_project_save and a milestone's with sona_milestone_save.",
    inputSchema: {
      type: "object",
      properties: {
        from: {
          type: "string",
          description: "First day shown: YYYY-MM-DD, today or tomorrow. The first of this month when left out.",
        },
        to: {
          type: "string",
          description: "Last day shown. Three months from from when left out.",
        },
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "timeline",
      ...assigned("--from", text(input, "from")),
      ...assigned("--to", text(input, "to")),
      "--json",
    ],
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
        dry_run: DRY_RUN,
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
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["project", "milestone"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "milestone-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
        dry_run: DRY_RUN,
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
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "name", true),
    ],
  },
  {
    name: "sona_person_merge",
    title: "Merge two people in Sona",
    description: `Fold one person into another: their tasks, meeting quotes and speaker links move to into, and person is gone. When both have a saved voice, voice says which one stays: into keeps into's voice, from keeps person's, both combines them; without it Sona refuses. Every choice but both deletes one saved voice for good. dry_run shows the person as the merge would leave them, and whether a voice choice is needed, without changing anything. ${CATALOG_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        person: { ...PERSON, description: `The person to fold away. ${PERSON.description}` },
        into: { ...PERSON, description: `The person who keeps everything. ${PERSON.description}` },
        voice: {
          type: "string",
          enum: ["into", "from", "both"],
          description: "When both people have a saved voice: into keeps into's, from keeps person's, both combines them.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["person", "into"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-merge",
      `--into=${text(input, "into", true)}`,
      ...assigned("--voice", text(input, "voice")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
    ],
  },
  {
    name: "sona_person_get",
    title: "Read one person's page in Sona",
    description:
      "Read one person's page in one call: their summary, role, organization, emails and notes, the meetings they are linked to (confirmed or suggested), their open commitments, the documents filed under them, and their contact card. Meetings kept on this Mac are left out, and so is the summary when one of them fed it (summary_left_out:true).",
    inputSchema: {
      type: "object",
      properties: { person: PERSON },
      required: ["person"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "person", "--json", "--", text(input, "person", true)],
  },
  {
    name: "sona_company_list",
    title: "List companies in Sona",
    description: "List the companies Sona knows from the organizations people carry, with how many people and meetings each has.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "companies", "--json"],
  },
  {
    name: "sona_company_get",
    title: "Read one company in Sona",
    description: "Read one company: its people, its meetings and its open commitments. Meetings kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: { company: { type: "string", description: "The company's name, as sona_company_list shows it." } },
      required: ["company"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "company", "--json", "--", text(input, "company", true)],
  },
  {
    name: "sona_person_first_met",
    title: "Read when you first met someone in Sona",
    description:
      "Read when the user first met a person, in which meeting, and who Sona thinks introduced them, with the suggestion still waiting for an answer. Left out when the first meeting is kept on this Mac (left_out:true).",
    inputSchema: {
      type: "object",
      properties: { person: PERSON },
      required: ["person"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "first-met", "--json", "--", text(input, "person", true)],
  },
  {
    name: "sona_person_first_met_answer",
    title: "Answer who introduced you to someone in Sona",
    description: `Answer who introduced the user to a person, as the person page's first-met card does: yes takes the introducer Sona heard, no turns it down. ${PEOPLE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        person: PERSON,
        answer: { type: "string", enum: ["yes", "no"], description: "Take (yes) or turn down (no) the introduction." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["person", "answer"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "first-met-answer",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
      text(input, "answer", true),
    ],
  },
  {
    name: "sona_person_link",
    title: "Link a person to a Sona meeting",
    description: `Link a person to a meeting, confirm a link Sona suggested with confirm:true, or take them off the meeting with remove:true, as the person page's meeting list does. ${PEOPLE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        person: PERSON,
        meeting_id: MEETING_ID,
        confirm: { type: "boolean", description: "Confirm the link Sona suggested." },
        remove: { type: "boolean", description: "Take the person off the meeting." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["person", "meeting_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-link",
      ...(flag(input, "confirm") ? ["--confirm"] : []),
      ...(flag(input, "remove") ? ["--remove"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
      text(input, "meeting_id", true),
    ],
  },
  {
    name: "sona_person_split",
    title: "Split a person in Sona",
    description: `Move some of a person's meetings, other names, emails or documents to another person (into) or a new one (new_person), as the person page's split sheet does. Give at least one of meetings, aliases, emails, documents. ${PEOPLE_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        person: PERSON,
        into: { ...PERSON, description: `The person who takes what moves. ${PERSON.description}` },
        new_person: { type: "string", description: "Add a person with this name to take what moves." },
        meetings: { type: "string", description: "Meeting ids to move, comma-separated." },
        aliases: { type: "string", description: "Other names to move, comma-separated." },
        emails: { type: "string", description: "Emails to move, comma-separated." },
        documents: { type: "string", description: "Document ids to move, comma-separated." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["person"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-split",
      ...assigned("--into", text(input, "into")),
      ...assigned("--new", text(input, "new_person")),
      ...assigned("--meetings", text(input, "meetings")),
      ...assigned("--aliases", text(input, "aliases")),
      ...assigned("--emails", text(input, "emails")),
      ...assigned("--documents", text(input, "documents")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
    ],
  },
  {
    name: "sona_person_delete",
    title: "Delete a person from Sona",
    description: `Delete a person with their meeting links and saved voice, which no tool brings back. ${PEOPLE_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { person: PERSON, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["person"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
    ],
  },
  {
    name: "sona_person_photo",
    title: "Set or remove a person's photo in Sona",
    description: `Set a person's photo from a PNG or JPEG file on this Mac (file), cropped to a square, or take it off with remove:true, which deletes the photo for good. ${PEOPLE_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        person: PERSON,
        file: { type: "string", description: "The absolute path of a PNG or JPEG on this Mac, at most 10 MB." },
        remove: { type: "boolean", description: "Take the photo off." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["person"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-photo",
      ...assigned("--file", text(input, "file")),
      ...(flag(input, "remove") ? ["--remove"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
    ],
  },
  {
    name: "sona_person_contact_remove",
    title: "Take the contact card off a person in Sona",
    description: `Take the contact card off a person, as the person page's remove contact does. ${PEOPLE_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { person: PERSON, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["person"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "person-contact-remove",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
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
      "List meetings, newest first: each one's id, title, start, minutes, whether it was processed, and the people in it. Read has_more before treating the list as complete; to see older meetings, raise limit, or narrow with since. title, status and folder narrow it the way the meetings list's search box, status filter and folder chips do.",
    inputSchema: {
      type: "object",
      properties: {
        since: {
          type: "string",
          description: "Only meetings from then on: a local day as YYYY-MM-DD, or milliseconds since the epoch.",
        },
        limit: LIMIT,
        title: { type: "string", description: "Only meetings whose title contains this, ignoring case." },
        status: {
          type: "string",
          enum: ["ready", "processing", "failed"],
          description: "Only meetings whose notes are ready, still being written, or failed.",
        },
        folder: { type: "string", description: "Only meetings filed in this folder, by name or id from sona_meeting_folders." },
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "meetings",
      ...assigned("--since", text(input, "since")),
      ...assigned("--limit", count(input, "limit")),
      ...assigned("--title", text(input, "title")),
      ...assigned("--status", text(input, "status")),
      ...assigned("--folder", text(input, "folder")),
      "--json",
    ],
  },
  {
    name: "sona_meeting_get",
    title: "Read a Sona meeting with its commitments",
    description:
      "Read one meeting: its summary, the commitments it left with who owes them, its speakers under meeting_speakers with the speaker_id sona_speaker_assign takes and needs_name for the ones nobody has named, and with transcript:true every line with who said it.",
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
  {
    name: "sona_work_delegate",
    title: "Hand a Sona task to an agent",
    description: `Hand one task to a named agent so the app shows who is working it, or take it back with to:"none". The answer carries the prompt to paste into that agent. Handing it to the agent already on it changes nothing and answers replayed:true. ${ORGANIZE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        to: {
          type: "string",
          description: 'The agent, as Sona shows it: "Claude Code", "Codex"; none takes the task back.',
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task", "to"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const to = text(input, "to", true);
      return [
        "work",
        "delegate",
        to.trim().toLowerCase() === "none" ? "--take-back" : `--to=${to}`,
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "task", true),
      ];
    },
  },
  {
    name: "sona_work_day",
    title: "Read a day's focus in Sona",
    description:
      "Read the up to three tasks picked as a day's focus, in order, and when they were picked.",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { day: DAY },
      additionalProperties: false,
    },
    argv: (input) => ["work", "day", "--json", "--", text(input, "day") ?? "today"],
  },
  {
    name: "sona_work_day_plan",
    title: "Pick a day's focus in Sona",
    description: `Pick up to three tasks as a day's focus, in order. They replace the day's earlier picks; tasks:"none" empties it. Picking what is already picked answers replayed:true. ${ORGANIZE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        tasks: { ...TASKS, description: `${TASKS.description} At most three, in order; none empties the day.` },
        day: DAY,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["tasks"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const picked = tasks(input, "tasks");
      const clear = picked.length === 1 && picked[0]?.toLowerCase() === "none";
      return [
        "work",
        "focus",
        ...assigned("--day", text(input, "day")),
        ...(clear ? ["--clear"] : []),
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        ...(clear ? [] : ["--", ...picked]),
      ];
    },
  },
  {
    name: "sona_work_keep",
    title: "Keep stale Sona tasks as they are",
    description: `Keep open tasks nobody has touched for 30 days as they are: they leave the stale list (sona_work_list with stale:true) without another change. ${ORGANIZE_WRITES}`,
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
      "keep",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      ...tasks(input, "tasks"),
    ],
  },
  {
    name: "sona_work_brief",
    title: "Brief before talking to someone",
    description:
      "One screen before talking to a person: the open work the user owes them and they owe the user (task handles), the talking points saved for them with their ids, and what was decided the last time they met.",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { person: PERSON },
      required: ["person"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "brief", "--json", "--", text(input, "person", true)],
  },
  {
    name: "sona_work_talking_point_add",
    title: "Add a Sona talking point",
    description: `Save something to raise the next time the user talks to a person; it shows in their brief. The same text for the same person on the same day lands once. ${ORGANIZE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        person: PERSON,
        text: { type: "string", description: "What to raise, up to 500 characters." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["person", "text"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "talking-point",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "person", true),
      text(input, "text", true),
    ],
  },
  {
    name: "sona_work_talking_point_done",
    title: "Mark a Sona talking point done",
    description: `Mark a talking point done, or not done with undo:true. A point already that way answers replayed:true. ${ORGANIZE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        id: { type: "string", description: "The talking point's id, from sona_work_brief." },
        undo: { type: "boolean", description: "Mark it not done again." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "talking-point-done",
      ...(flag(input, "undo") ? ["--undo"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "id", true),
    ],
  },
  {
    name: "sona_meeting_loop_reopen",
    title: "Reopen a Sona meeting loop",
    description: `Reopen a meeting loop that was resolved or set aside; a task tracking it reopens with it. A loop already open answers replayed:true. ${LOOP_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { loop_id: LOOP_ID, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["loop_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "loop-reopen",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "loop_id", true),
    ],
  },
  {
    name: "sona_meeting_loop_assign",
    title: "Set a Sona meeting loop's owner",
    description: `Set who owns a meeting loop, or owner:"none" to clear it. The same owner again answers replayed:true. ${LOOP_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        loop_id: LOOP_ID,
        owner: { ...PERSON, description: `${PERSON.description} none clears the owner.` },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["loop_id", "owner"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "loop-owner",
      `--owner=${text(input, "owner", true)}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "loop_id", true),
    ],
  },
  {
    name: "sona_speaker_assign",
    title: "Say who a speaker in a Sona meeting is",
    description: `Say who one of a meeting's speakers is, as the speaker menu in the transcript does: person names someone already in Sona, new_person adds a person with that name, and undo:true marks the speaker unknown the way Mark as unknown does. Give exactly one of the three. speaker is the name the transcript shows ("Speaker 2") or a speaker_id from sona_meeting_get. Changing who a named speaker is, or undo, deletes the voice samples Sona kept from that speaker, which no tool brings back. Sona never remembers the voice from this call: saving a voice profile stays a step the user takes in the app. A meeting kept on this Mac answers not_found. A speaker that already reads that way answers replayed:true. It needs Settings > Agents > External access and External mutations, and Agents' changes set to Apply: with access or mutations off Sona refuses with consent_required, and while agents' changes wait for the user it refuses with invalid_request naming the setting. dry_run:true shows the result without writing. Repeating an operation_id answers the first result again. The meeting's history names the user, not the agent, as the one who named the speaker.`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        meeting_id: MEETING_ID,
        speaker: {
          type: "string",
          description: 'The speaker as the transcript names them, such as "Speaker 2", or their speaker_id from sona_meeting_get.',
        },
        person: PERSON,
        new_person: { type: "string", description: "Add a person with this name and say the speaker is them." },
        undo: { type: "boolean", description: "Mark the speaker unknown again." },
        same_voice: {
          type: "boolean",
          description:
            "Name the same voice too in the other meetings Sona heard it in where nobody named it (see sona_speaker_voices). Meetings kept on this Mac are left alone.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["meeting_id", "speaker"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "speaker-assign",
      ...assigned("--person", text(input, "person")),
      ...assigned("--new", text(input, "new_person")),
      ...(flag(input, "undo") ? ["--undo"] : []),
      ...(flag(input, "same_voice") ? ["--same-voice"] : []),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "meeting_id", true),
      text(input, "speaker", true),
    ],
  },
  {
    name: "sona_speaker_voices",
    title: "Read what voice matching says about a Sona meeting",
    description:
      "Read what voice matching says about a meeting's speakers: the person it suggests for each waiting speaker, the other meetings it heard each unnamed voice in (meetings kept on this Mac left out), and the speakers it named by itself with the name each had before.",
    inputSchema: {
      type: "object",
      properties: { meeting_id: MEETING_ID },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "speaker-voices", "--json", "--", text(input, "meeting_id", true)],
  },
  {
    name: "sona_speaker_confirm",
    title: "Take voice matching's suggestion for a speaker",
    description: `Take the person voice matching suggests for a waiting speaker, as the suggestion button in the transcript does; for the user's own voice it names the speaker with the user's name. speaker is the name the transcript shows or a speaker_id from sona_speaker_voices. A meeting kept on this Mac answers not_found. ${ORGANIZE_WRITES} The meeting's history names the user.`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        meeting_id: MEETING_ID,
        speaker: { type: "string", description: "The speaker as the transcript names them, or their speaker_id." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["meeting_id", "speaker"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "speaker-confirm",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "meeting_id", true),
      text(input, "speaker", true),
    ],
  },
  {
    name: "sona_speaker_voice_undo",
    title: "Undo a name voice matching gave a speaker",
    description: `Give a speaker that voice matching named by itself back the name it had, as the transcript's undo does. A meeting kept on this Mac answers not_found. ${ORGANIZE_WRITES} The meeting's history names the user.`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        meeting_id: MEETING_ID,
        speaker: { type: "string", description: "The speaker as the transcript names them, or their speaker_id." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["meeting_id", "speaker"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "speaker-voice-undo",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "meeting_id", true),
      text(input, "speaker", true),
    ],
  },
  ...meetingLibraryTools(),
  {
    name: "sona_planning_track_action",
    title: "Track a meeting action as a Sona task",
    description: `Make a task in a project from an action a meeting left with no task; the answer lists the task. ${ORGANIZE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        loop_id: LOOP_ID,
        project: PROJECT,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["loop_id", "project"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "action-track",
      `--project=${text(input, "project", true)}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "loop_id", true),
    ],
  },
  actionAside("dismiss", "Set a meeting action aside", "Set an action a meeting left with no task aside as not work, so planning stops asking about it."),
  actionAside("restore", "Ask about a meeting action again", "Bring back an action set aside as not work, so planning asks about it again."),
  {
    name: "sona_scratchpad_create",
    title: "Make a Sona note",
    description: `Make a note; its first words become its title. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        body: { type: "string", description: "The note's text." },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: NOTE_OPERATION },
      },
      required: ["body"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "note-new",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "body", true),
    ],
  },
  {
    name: "sona_scratchpad_update_body",
    title: "Change a Sona note",
    description: `Replace a note's whole body; the old body stays as a version the user can put back. The same body again answers replayed:true. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        note_id: NOTE_ID,
        body: { type: "string", description: "The whole new body." },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: NOTE_OPERATION },
      },
      required: ["note_id", "body"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "note-edit",
      `--body=${text(input, "body") ?? ""}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "note_id", true),
    ],
  },
  {
    name: "sona_scratchpad_set_pinned",
    title: "Pin a Sona note",
    description: `Pin a note, or unpin it with pinned:false. A note already that way answers replayed:true. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        note_id: NOTE_ID,
        pinned: { type: "boolean", description: "true pins, false unpins." },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: NOTE_OPERATION },
      },
      required: ["note_id", "pinned"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "note-pin",
      ...(flag(input, "pinned") ? [] : ["--unpin"]),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "note_id", true),
    ],
  },
  {
    name: "sona_scratchpad_link",
    title: "Link to a Sona note",
    description: `Link a meeting, a person or another note to a note, as the note's sources. A link already there answers replayed:true. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        note_id: NOTE_ID,
        kind: {
          type: "string",
          enum: ["meeting", "person", "note"],
          description: "What to link: a meeting, a person, or another note.",
        },
        source_id: {
          type: "string",
          description:
            "Its id: a meeting's uuid from sona_meeting_list, a person's uuid from sona_person_list, or another note's id.",
        },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: NOTE_OPERATION },
      },
      required: ["note_id", "kind", "source_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const kind = text(input, "kind", true);
      if (kind !== "meeting" && kind !== "person" && kind !== "note") {
        throw new SonaInputError("kind must be meeting, person or note.");
      }
      return [
        "work",
        "note-link",
        `--${kind}=${text(input, "source_id", true)}`,
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "note_id", true),
      ];
    },
  },
  {
    name: "sona_scratchpad_unlink",
    title: "Unlink a source from a Sona note",
    description: `Take a linked meeting, person or note off a note's sources. A source not linked answers replayed:true. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        note_id: NOTE_ID,
        kind: {
          type: "string",
          enum: ["meeting", "person", "note"],
          description: "What to unlink: a meeting, a person, or another note.",
        },
        source_id: { type: "string", description: "Its id, as the note's sources list it." },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: NOTE_OPERATION },
      },
      required: ["note_id", "kind", "source_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const kind = text(input, "kind", true);
      if (kind !== "meeting" && kind !== "person" && kind !== "note") {
        throw new SonaInputError("kind must be meeting, person or note.");
      }
      return [
        "work",
        "note-unlink",
        `--${kind}=${text(input, "source_id", true)}`,
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "note_id", true),
      ];
    },
  },
  {
    name: "sona_scratchpad_delete",
    title: "Delete a Sona note",
    description: `Delete a note and every saved version of it, and take it off the meetings it was linked to. It cannot be undone. Repeating the call answers replayed:true. ${NOTE_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { note_id: NOTE_ID, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["note_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "note-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "note_id", true),
    ],
  },
  {
    name: "sona_scratchpad_restore_version",
    title: "Restore a version of a Sona note",
    description: `Put one of a note's saved versions back as its body. The body it replaces stays as a version. dry_run:true returns the body it would put back under would_write. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        note_id: NOTE_ID,
        version_id: { type: "integer", minimum: 1, description: "A saved version id from sona_note_versions." },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: NOTE_OPERATION },
      },
      required: ["note_id", "version_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "note-restore",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "note_id", true),
      requiredCount(input, "version_id"),
    ],
  },
  {
    name: "sona_scratchpad_subjects",
    title: "List Sona note subjects",
    description: "List the subjects notes gather under, each with its id and the note that is current for it.",
    optIn: true,
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "subjects", "--json"],
  },
  {
    name: "sona_scratchpad_subject_create",
    title: "Make a Sona note subject",
    description: `Make a subject notes can gather under. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        title: { type: "string", description: "The subject's name." },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: NOTE_OPERATION },
      },
      required: ["title"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "subject-new",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "title", true),
    ],
  },
  {
    name: "sona_scratchpad_subject_note",
    title: "Put a Sona note under a subject",
    description: `Add a note to a subject, make it the subject's current note, or take it off. Already so answers replayed:true. ${NOTE_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        subject_id: { type: "string", description: "A subject's id from sona_scratchpad_subjects." },
        note_id: NOTE_ID,
        action: {
          type: "string",
          enum: ["add", "current", "remove"],
          description: "add puts it under the subject, current also makes it the current note, remove takes it off. add unless told.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["subject_id", "note_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const action = text(input, "action") ?? "add";
      if (action !== "add" && action !== "current" && action !== "remove") {
        throw new SonaInputError("action must be add, current or remove.");
      }
      return [
        "work",
        "subject-note",
        ...(action === "add" ? [] : [`--${action}`]),
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "subject_id", true),
        text(input, "note_id", true),
      ];
    },
  },
  {
    name: "sona_work_parse",
    title: "Read words as Sona task fields",
    description:
      "Read a line as the task box reads it, writing nothing: the title left, and each due date, priority, label, owner, project and repeat the words name, with where they sit. People are read only while the conversations are open.",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        words: { type: "string", description: "The line: \"call Dana friday p1 #design\"." },
        spoken: { type: "boolean", description: "Read it as said aloud: no sigils, the owner named first." },
        plain: {
          type: "array",
          items: { type: "string" },
          description: "Words to keep as plain words, read as nothing.",
        },
      },
      required: ["words"],
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "parse",
      ...(flag(input, "spoken") ? ["--spoken"] : []),
      ...(input.plain === undefined ? [] : strings(input, "plain").map((word) => `--plain=${word}`)),
      "--json",
      "--",
      text(input, "words", true),
    ],
  },
  ...(["book", "unbook"] as const).map(
    (verb): ToolDefinition => ({
      name: `sona_work_${verb}`,
      title: verb === "book" ? "Book time for a Sona task" : "Unbook a Sona task's time",
      description:
        verb === "book"
          ? `Book the next free working hour before the task is due, or in the coming week, in the user's "Sona" calendar. A task with an hour ahead keeps it and answers replayed:true. Needs calendar actions on in Settings > Connections and Calendar access for the process running sona. ${WRITES}`
          : `Take a task's booked hour off the user's calendar. Nothing booked answers replayed:true. ${WRITES}`,
      mutates: "changes",
      optIn: true,
      inputSchema: {
        type: "object",
        properties: { task: TASK, dry_run: DRY_RUN, operation_id: OPERATION_ID },
        required: ["task"],
        additionalProperties: false,
      },
      argv: (input, agent) => [
        "work",
        verb,
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "task", true),
      ],
    }),
  ),
  {
    name: "sona_work_nudged",
    title: "Mark a Sona wait as asked about",
    description: `Mark a task waiting on a person as asked about today, so its follow-up comes back a week from now. Already asked today answers replayed:true. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { task: TASK, dry_run: DRY_RUN, operation_id: OPERATION_ID },
      required: ["task"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "nudged",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
    ],
  },
  {
    name: "sona_work_watch_task",
    title: "Make a Sona task from a watched line",
    description: `Make a Triage task from a line a watch heard, quoting it as the task's source. A line already made a task answers that task with replayed:true. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        watch: { type: "string", description: "The watched topic or its id, from sona_work_watches." },
        line: { type: "integer", minimum: 1, description: "The heard line's number, as sona_work_watches lists it." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["watch", "line"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "watch-task",
      `--line=${requiredCount(input, "line")}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "watch", true),
    ],
  },
  {
    name: "sona_work_link_suggestions",
    title: "List Sona's link suggestions",
    description:
      "List the links Sona suggests between meetings, notes and work: each suggestion's id, kind, target, source and the passage behind it. Pending ones unless all. Ones drawn from meetings kept on this Mac are left out.",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { all: { type: "boolean", description: "Also list accepted and dismissed ones." } },
      additionalProperties: false,
    },
    argv: (input) => ["work", "link-suggestions", ...(flag(input, "all") ? ["--all"] : []), "--json"],
  },
  ...(["accept", "dismiss"] as const).map(
    (verb): ToolDefinition => ({
      name: `sona_work_link_${verb}`,
      title: verb === "accept" ? "Accept a Sona link suggestion" : "Dismiss a Sona link suggestion",
      description: `${verb === "accept" ? "Accept a suggested link, keeping it as evidence on its target" : "Dismiss a suggested link"}. Already ${verb === "accept" ? "accepted" : "dismissed"} answers replayed:true. ${WRITES}`,
      mutates: "changes",
      optIn: true,
      inputSchema: {
        type: "object",
        properties: {
          id: { type: "string", description: "The suggestion's id from sona_work_link_suggestions." },
          dry_run: DRY_RUN,
          operation_id: OPERATION_ID,
        },
        required: ["id"],
        additionalProperties: false,
      },
      argv: (input, agent) => [
        "work",
        `link-${verb}`,
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "id", true),
      ],
    }),
  ),
  {
    name: "sona_work_triage_suggestions",
    title: "Read Sona's triage suggestions",
    description:
      "List what Sona suggests for tasks in Triage: a project, labels, an owner, a due day, and a task each one may repeat or relate to. Each suggestion has an id and a reason in plain words. Every task in Triage unless task names one. Suggestions drawn from meetings kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: { task: { ...TASK, description: `Only this task. ${TASK.description}` } },
      additionalProperties: false,
    },
    argv: (input) => {
      const task = text(input, "task");
      return ["work", "triage-suggestions", "--json", ...(task === null ? [] : ["--", task])];
    },
  },
  ...(["accept", "dismiss"] as const).map(
    (verb): ToolDefinition => ({
      name: `sona_work_suggestion_${verb}`,
      title: verb === "accept" ? "Accept a Sona triage suggestion" : "Dismiss a Sona triage suggestion",
      description: `${verb === "accept" ? "Accept a triage suggestion: the task changes as it would through sona_work_update, sona_work_relate or a duplicate decision" : "Dismiss a triage suggestion; it stays dismissed"}. ${WRITES}`,
      mutates: "changes",
      inputSchema: {
        type: "object",
        properties: {
          id: { type: "string", description: "The suggestion's id from sona_work_triage_suggestions." },
          dry_run: DRY_RUN,
          operation_id: OPERATION_ID,
        },
        required: ["id"],
        additionalProperties: false,
      },
      argv: (input, agent) => [
        "work",
        `suggestion-${verb}`,
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "id", true),
      ],
    }),
  ),
  {
    name: "sona_work_calendar",
    title: "Read Sona's planner calendar",
    description:
      "Read tasks, milestones and meetings by day, as the planner calendar shows them. Seven days from today unless told.",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        from: DAY,
        to: { type: "string", description: "The last day, included: YYYY-MM-DD. Six days after from unless told." },
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "calendar",
      ...assigned("--from", text(input, "from")),
      ...assigned("--to", text(input, "to")),
      "--json",
    ],
  },
  {
    name: "sona_work_trend",
    title: "Read Sona's stats trend",
    description:
      "Read dictation and meeting totals by local day over the last 7, 30 or 180 days: dictations, words and spoken time, active days and streak, and meetings, captured time and action items.",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        range: { type: "string", enum: ["7", "30", "180"], description: "How many days back, today included. 30 unless told." },
      },
      additionalProperties: false,
    },
    argv: (input) => ["work", "trend", ...assigned("--range", text(input, "range")), "--json"],
  },
  {
    name: "sona_work_today",
    title: "Read Sona's Today strip",
    description:
      "Read Home's Today strip: today's meetings and up to four known people in them (absent with no calendar connected), meetings still processing, speakers nobody named, promises not tracked, and the loops the user owes with how many are late. Each count says unavailable when its source could not be read. Meetings kept on this Mac are left out.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "today", "--json"],
  },
  {
    name: "sona_work_plan_accept",
    title: "Make a Sona project from a plan draft",
    description: `Make a project with its milestones and tasks from a Plan with Sona draft: one object with goal, sources, project, milestones, tasks and linkedCommitments, in camelCase, as the app's planner makes it. A draft you write yourself leaves sources and every citations list empty. A draft of another shape is refused with what is wrong. Citations must point at the draft's own sources and passages. The same draft with the same operation_id returns the same project. ${WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        draft: { type: "string", description: "The plan draft as JSON text." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["draft"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "plan-accept",
      `--draft=${text(input, "draft", true)}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
    ],
  },
  {
    name: "sona_work_attachment_rename",
    title: "Rename a Sona task's attachment",
    description: `Rename a file or link attached to a task. The same title again answers replayed:true. ${COMMENT_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        id: ATTACHMENT,
        title: { type: "string", description: "The new title." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task", "id", "title"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "attachment-rename",
      `--title=${text(input, "title", true)}`,
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
      text(input, "id", true),
    ],
  },
  {
    name: "sona_work_capture",
    title: "Capture a Sona task from a page",
    description: `Create a task with the page or file it came from kept as its source, as the app's capture does. ${WRITES} Calling again with the same arguments on the same day returns the first task with replayed:true. dry_run:true shows the task without creating it.`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        title: { type: "string", description: "The task's title." },
        url: { type: "string", description: "The page or file: an http, https or file address." },
        url_title: { type: "string", description: "What the page or file is called." },
        ...FIELDS,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["title", "url"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "create",
      `--source-url=${text(input, "url", true)}`,
      ...assigned("--source-title", text(input, "url_title")),
      ...fieldFlags(input),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "title", true),
    ],
  },
  {
    name: "sona_task_save",
    title: "Change a Sona task in one save",
    description: `Change one task with an ordered list of steps, applied together or not at all: a step that cannot apply refuses the whole save, naming it as patch[n]. Each step is {field, value}; a null value clears the field. labels takes op set, add or remove with a list of names or ids. description takes op replace (default), append, prepend, insert_before, insert_after or replace_text; the last three need anchor, text already in the description, and refuse when it is not there. Read the task with sona_work_get first and pass its revision as expected_revision so a change made meanwhile is refused instead of overwritten. ${WRITES} dry_run:true shows the result field by field without writing. Repeating the same save answers the first result again.`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        patch: {
          type: "array",
          items: PATCH_STEP,
          minItems: 1,
          description:
            'The steps, in order: [{"field":"status","value":"in_progress"},{"field":"labels","op":"add","value":["bug"]},{"field":"description","op":"append","value":"Found the cause."}].',
        },
        expected_revision: {
          type: "integer",
          description: "The revision sona_work_get returned; the save is refused if the task changed since.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task", "patch"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "save",
      ...objects("--patch-json", input, "patch", true),
      ...assigned("--expected-revision", count(input, "expected_revision")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
    ],
  },
  {
    name: "sona_session_start",
    title: "Start an agent session on a Sona task",
    description: `Start your session on a task: the run the user watches in Sona, with your thoughts, actions, questions and plan, and which the user can stop. While the user lets agents' changes apply you become the task's delegate when it has none; the owner stays the user's. A task delegated to another agent is refused. Write within 10 seconds (sona_session_activity) or the session shows as unresponsive, and keep writing or send sona_session_heartbeat: after 30 minutes of silence it shows as stale. End it with a response or an error activity. ${COMMENT_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        task: TASK,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["task"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "session-start",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "task", true),
    ],
  },
  {
    name: "sona_session_activity",
    title: "Write to your Sona agent session",
    description: `Add one activity to a session you started. thought: what you are thinking. action: a tool use, with action (what it does), parameter (what on) and result once known. elicitation: a question for the user; options makes it multiple choice and the session waits in awaiting_input until the user answers. response: your final answer, which completes the session. error: what stopped you, which ends it as failed. ephemeral:true marks a thought or action the next activity replaces. plan replaces the session's checklist in the same write. Every answer carries stop_requested, unread_prompts and an instruction: when the user asked you to stop, only a response or an error is accepted. ${COMMENT_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        session: SESSION,
        type: {
          type: "string",
          enum: ["thought", "action", "elicitation", "response", "error"],
          description: "What kind of activity this is.",
        },
        body: { type: "string", description: "The text, in Markdown. Every type but action needs it." },
        action: { type: "string", description: "For an action: what it does, such as Searching or Editing." },
        parameter: { type: "string", description: "For an action: what it acts on." },
        result: { type: "string", description: "For an action: what came of it." },
        options: {
          type: "array",
          items: SELECT_OPTION,
          description: "For an elicitation: the choices, each with a label to show and a value you get back.",
        },
        ephemeral: { type: "boolean", description: "A thought or action the next activity replaces." },
        plan: PLAN,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["session", "type"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "session-activity",
      `--type=${text(input, "type", true)}`,
      ...assigned("--body", text(input, "body")),
      ...assigned("--action", text(input, "action")),
      ...assigned("--parameter", text(input, "parameter")),
      ...assigned("--result", text(input, "result")),
      ...objects("--options-json", input, "options", false),
      ...(flag(input, "ephemeral") ? ["--ephemeral"] : []),
      ...objects("--plan-json", input, "plan", false),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "session", true),
    ],
  },
  {
    name: "sona_session_plan",
    title: "Set your Sona session's plan",
    description: `Replace the plan of a session you started: the whole checklist, every step with its status. Send it again as steps move. ${COMMENT_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        session: SESSION,
        plan: PLAN,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["session", "plan"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "session-plan",
      ...objects("--plan-json", input, "plan", true),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "session", true),
    ],
  },
  {
    name: "sona_session_heartbeat",
    title: "Tell Sona your session is alive",
    description: `Mark a session you started as still working, between activities, so it does not show as stale. The answer says whether the user asked you to stop (stop_requested, with an instruction) and carries anything the user wrote to you (unread_prompts). ${COMMENT_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        session: SESSION,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["session"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "session-heartbeat",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "session", true),
    ],
  },
  {
    name: "sona_session_get",
    title: "Read a Sona agent session",
    description:
      "One agent session: its task, agent, state (pending, active, awaiting_input, stale, complete or error), whether it is unresponsive, the plan, the open question, whether a stop was requested, prompts the agent has not answered yet, and every activity oldest first.",
    inputSchema: {
      type: "object",
      properties: { session: SESSION },
      required: ["session"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "session", "--json", "--", text(input, "session", true)],
  },
  {
    name: "sona_session_list",
    title: "List Sona agent sessions",
    description:
      "Agent sessions newest first, each with its task, agent, state, plan and open question: one task's, or every task's. Sessions on work kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: {
        task: { ...TASK, description: `Only this task's sessions. ${TASK.description}` },
        open: { type: "boolean", description: "Only sessions not yet ended." },
        agent: { type: "string", description: "Only this agent's sessions, by the name it gave." },
      },
      additionalProperties: false,
    },
    argv: (input) => {
      const task = text(input, "task");
      return [
        "work",
        "sessions",
        ...(flag(input, "open") ? ["--open"] : []),
        ...assigned("--agent", text(input, "agent")),
        "--json",
        ...(task === null ? [] : ["--", task]),
      ];
    },
  },
  {
    name: "sona_session_stop",
    title: "Ask an agent to stop its Sona session",
    description: `Record a stop request on a session: the agent running it sees it on its next write or heartbeat and must end with a response or an error. ${COMMENT_WRITES}`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        session: SESSION,
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["session"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "session-stop",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "session", true),
    ],
  },
  {
    name: "sona_work_changes",
    title: "Follow changes in Sona's tracker",
    description:
      "What changed on tasks, projects, comments and agent sessions, oldest first, each with a seq, what changed, and under updated_from the fields' values before the change. Pass the answer's next_cursor as since to read only what came after; has_more says another page is waiting. Changes are kept 30 days: an older cursor is refused as expired, and the fix is to read the tasks again and follow from a fresh start. Changes on work kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: {
        since: { type: "integer", description: "The next_cursor of the last answer. Left out: from the oldest change kept." },
        limit: LIMIT,
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "changes",
      ...assigned("--since", count(input, "since")),
      ...assigned("--limit", count(input, "limit")),
      "--json",
    ],
  },
  {
    name: "sona_planning_link_source",
    title: "Link a meeting, note or loop to a Sona task or project",
    description: `Link a meeting, a note or a meeting loop to a task or a project as its source, as the Sources section of a task or project does. kind says what source_id is. at_seconds points at one moment of a meeting. A meeting or loop kept on this Mac answers not_found. A source already linked answers replayed:true. ${PLANNING_WRITES} The link records no author.`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "A task (S-12, its number or id), or a project's name or id." },
        kind: { type: "string", enum: ["meeting", "note", "loop"], description: "What source_id names." },
        source_id: { type: "string", description: "The meeting's uuid, the note's id, or the loop's id from sona_action_items." },
        at_seconds: { type: "integer", description: "With kind meeting: the moment to point at, in seconds from the meeting's start." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["target", "kind", "source_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const kind = text(input, "kind", true);
      if (kind !== "meeting" && kind !== "note" && kind !== "loop") {
        throw new SonaInputError("kind must be meeting, note or loop.");
      }
      return [
        "work",
        "source-link",
        `--${kind}=${text(input, "source_id", true)}`,
        ...assigned("--at", count(input, "at_seconds")),
        ...(flag(input, "dry_run") ? ["--dry-run"] : []),
        ...writing(input, agent),
        "--json",
        "--",
        text(input, "target", true),
      ];
    },
  },
  {
    name: "sona_ask_pack",
    title: "Gather the evidence Sona's Ask would answer from",
    description:
      "The evidence Sona's Ask hands its model for a question: a card of what Sona holds, then the matching meeting lines, notes and dictations, each quoted with its sona:// link. folder narrows it to the meetings filed in one folder. Answer the question yourself from the pack and cite the links; Sona runs no model here. Meetings kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: {
        question: { type: "string", description: "The question, in words." },
        folder: { type: "string", description: "A meeting folder's name or id." },
      },
      required: ["question"],
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "ask-pack",
      ...assigned("--folder", text(input, "folder")),
      "--json",
      "--",
      text(input, "question", true),
    ],
  },
  {
    name: "sona_documents",
    title: "List documents added to Sona",
    description:
      "Documents added to Sona, newest first, with their text: all of them, the ones that name person, or the ones added to meeting_id, each of those with what was said in the meeting that differs from what the document says. Documents added to a meeting kept on this Mac are left out.",
    inputSchema: {
      type: "object",
      properties: {
        person: PERSON,
        meeting_id: MEETING_ID,
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "docs",
      ...assigned("--person", text(input, "person")),
      ...assigned("--meeting", text(input, "meeting_id")),
      "--json",
    ],
  },
  {
    name: "sona_document_add",
    title: "Add a document to Sona",
    description: `Add a .txt, .md, .markdown or .pdf file as a document, on its own or to meeting_id, as the app's document import does. Sona then links the people it names. A meeting kept on this Mac answers not_found. ${DOCUMENT_WRITES} Repeating an operation_id answers the first document with replayed:true. The document records no author.`,
    mutates: "changes",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        path: { type: "string", description: "The file's absolute path." },
        meeting_id: MEETING_ID,
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: DOCUMENT_OPERATION },
      },
      required: ["path"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "doc-add",
      ...assigned("--meeting", text(input, "meeting_id")),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "path", true),
    ],
  },
  {
    name: "sona_document_delete",
    title: "Delete a document from Sona",
    description: `Delete a document by its id from sona_documents, as the app's delete does. Nothing brings it back. ${DOCUMENT_WRITES} Repeating an operation_id answers replayed:true.`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        document_id: { type: "string", description: "The document's id, from sona_documents." },
        dry_run: DRY_RUN,
        operation_id: { ...OPERATION_ID, description: DOCUMENT_OPERATION },
      },
      required: ["document_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "doc-delete",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "document_id", true),
    ],
  },
  {
    name: "sona_settings",
    title: "Read Sona's settings",
    description:
      "Sona's settings an agent may see: dictation and sounds, microphones, appearance, log level and debug mode, the cleanup provider and model, modes, dictation prompts, vocabulary, snippets and text replacements, shortcuts with their default keys, the evening digest, keyword trackers and meeting prep. API keys, consent and privacy switches are left out.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "settings", "--json"],
  },
  {
    name: "sona_setting",
    title: "Change one Sona setting",
    description: `Change one setting as its switch or picker in Settings does: true or false for a switch, a name for a choice, JSON for keyword-trackers (the whole list) and meeting-prep (the whole object; the mail and web switches stay the user's). mode is the capture mode chip; dictation-prompt uses a prompt. Consent, privacy, API keys and offline cleanup stay in Sona. ${SETTINGS_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        key: { type: "string", enum: [...SETTING_KEYS], description: "Which setting." },
        value: { type: "string", description: "The new value." },
        provider: {
          type: "string",
          description: "For cleanup-model and cleanup-base-url: the provider's id. Left out: the selected one.",
        },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["key", "value"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const value = z.string().safeParse(input.value);
      if (!value.success) throw new SonaInputError("value must be a string.");
      return [
        ...settingsWrite("setting", input, agent),
        ...assigned("--provider", text(input, "provider")),
        "--",
        text(input, "key", true),
        value.data,
      ];
    },
  },
  {
    name: "sona_setting_reset",
    title: "Reset a shortcut or the text replacements",
    description: `Put one shortcut back to its default key, or replace every text replacement with Sona's starter library. ${SETTINGS_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", enum: ["shortcut", "replacements"], description: "What to reset." },
        id: { type: "string", description: "The shortcut's id, from sona_settings under shortcuts." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["target"],
      additionalProperties: false,
    },
    argv: (input, agent) => {
      const id = text(input, "id");
      return [
        ...settingsWrite("setting-reset", input, agent),
        "--",
        text(input, "target", true),
        ...(id === null ? [] : [id]),
      ];
    },
  },
  {
    name: "sona_mode_save",
    title: "Make or change a Sona mode",
    description: `Save a whole mode, as JSON: one entry of sona_settings modes.modes, changed, without shortcuts. A new id makes a new mode. ${SETTINGS_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        mode: {
          type: "string",
          description: "The mode as JSON: id, name, tone, context_policy, asr, llm, prompt, delivery.",
        },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["mode"],
      additionalProperties: false,
    },
    argv: (input, agent) => [...settingsWrite("mode-save", input, agent), `--mode-json=${text(input, "mode", true)}`],
  },
  {
    name: "sona_mode_delete",
    title: "Delete a Sona mode",
    description: `Delete a mode and its shortcuts. The default mode stays. ${SETTINGS_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: { mode: MODE_ID, dry_run: DRY_RUN, operation_id: SETTINGS_OPERATION_ID },
      required: ["mode"],
      additionalProperties: false,
    },
    argv: (input, agent) => [...settingsWrite("mode-delete", input, agent), "--", text(input, "mode", true)],
  },
  {
    name: "sona_mode_reorder",
    title: "Reorder Sona's modes",
    description: `Put the modes in this order; every mode's id, once. ${SETTINGS_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        modes: { type: "array", items: { type: "string" }, description: "Every mode's id, in the new order." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["modes"],
      additionalProperties: false,
    },
    argv: (input, agent) => [...settingsWrite("mode-reorder", input, agent), "--", ...strings(input, "modes")],
  },
  {
    name: "sona_prompt_save",
    title: "Add or change a dictation prompt",
    description: `Add a dictation prompt, or change one by id. Use one with sona_setting key dictation-prompt. ${SETTINGS_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The prompt's name." },
        prompt: { type: "string", description: "The prompt's text." },
        id: { type: "string", description: "The prompt to change. Left out: a new prompt." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["name", "prompt"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("prompt-save", input, agent),
      `--prompt=${text(input, "prompt", true)}`,
      ...assigned("--id", text(input, "id")),
      "--",
      text(input, "name", true),
    ],
  },
  {
    name: "sona_prompt_delete",
    title: "Delete a dictation prompt",
    description: `Delete a dictation prompt by id. The last one stays. ${SETTINGS_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        id: { type: "string", description: "The prompt's id, from sona_settings." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [...settingsWrite("prompt-delete", input, agent), "--", text(input, "id", true)],
  },
  {
    name: "sona_vocabulary_add",
    title: "Add a vocabulary word",
    description: `Teach dictation a word: what it hears, and how to write it. For one mode with mode, else every mode's vocabulary. ${SETTINGS_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        spoken: { type: "string", description: "What dictation hears." },
        written: { type: "string", description: "How to write it." },
        mode: MODE_ID,
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["spoken", "written"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("vocabulary-add", input, agent),
      ...assigned("--mode", text(input, "mode")),
      "--",
      text(input, "spoken", true),
      text(input, "written", true),
    ],
  },
  {
    name: "sona_vocabulary_csv",
    title: "Import vocabulary from a CSV file",
    description: `Add every row of a spoken,written CSV file to the vocabulary, as Import does. A file with a bad, repeated or conflicting row adds nothing. ${SETTINGS_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        file: { type: "string", description: "The CSV file's path on this Mac." },
        mode: MODE_ID,
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["file"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("vocabulary-csv", input, agent),
      ...assigned("--mode", text(input, "mode")),
      "--",
      text(input, "file", true),
    ],
  },
  {
    name: "sona_workflows",
    title: "Read Sona's background workflows",
    description:
      "The background workflows, each on or off with its last run; with runs, recent runs newest first. Runs show status, times and counts, not what they touched.",
    inputSchema: {
      type: "object",
      properties: {
        runs: { type: "boolean", description: "List recent runs instead." },
        workflow: { type: "string", description: "Only this workflow's runs, by id." },
        limit: LIMIT,
      },
      additionalProperties: false,
    },
    argv: (input) => [
      "work",
      "workflows",
      ...(flag(input, "runs") ? ["--runs"] : []),
      ...assigned("--workflow", text(input, "workflow")),
      ...assigned("--limit", count(input, "limit")),
      "--json",
    ],
  },
  {
    name: "sona_workflow_enable",
    title: "Turn a background workflow on or off",
    description:
      "Turn one background workflow on, or off with off. Needs Sona's outside changes allowed, and obeys its Apply or Suggest setting.",
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        workflow: { type: "string", description: "The workflow's id, from sona_workflows." },
        off: { type: "boolean", description: "Turn it off." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["workflow"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("workflow-enable", input, agent),
      ...(flag(input, "off") ? ["--off"] : []),
      "--",
      text(input, "workflow", true),
    ],
  },
  {
    name: "sona_saved_prompts",
    title: "Read Sona's saved prompts",
    description: "The saved prompts: each one's id, name, question, answer shape, and whether it is asked about a meeting, a person or a series.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "saved-prompts", "--json"],
  },
  {
    name: "sona_saved_prompt_save",
    title: "Save a saved prompt",
    description: `Save a question to ask about a meeting, a person or a series, or change one by id. Asking it runs in Sona. ${SETTINGS_STORE_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The prompt's name." },
        body: { type: "string", description: "What to ask." },
        about: { type: "string", enum: ["meeting", "person", "series"], description: "What the prompt is asked about." },
        schema: { type: "string", description: "Answer as one JSON object checked against this JSON schema, instead of prose." },
        id: { type: "string", description: "The saved prompt to change, from sona_saved_prompts. Left out: a new prompt." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["name", "body", "about"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("saved-prompt-save", input, agent),
      `--body=${text(input, "body", true)}`,
      `--about=${text(input, "about", true)}`,
      ...assigned("--schema", text(input, "schema")),
      ...assigned("--id", text(input, "id")),
      "--",
      text(input, "name", true),
    ],
  },
  {
    name: "sona_saved_prompt_delete",
    title: "Delete a saved prompt",
    description: `Delete one saved prompt. ${SETTINGS_STORE_WRITES}`,
    mutates: "destroys",
    optIn: true,
    inputSchema: {
      type: "object",
      properties: {
        id: { type: "string", description: "The saved prompt's id, from sona_saved_prompts." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [...settingsWrite("saved-prompt-delete", input, agent), "--", text(input, "id", true)],
  },
  {
    name: "sona_series",
    title: "Read a meeting's series settings",
    description: "A meeting's calendar series: its notes template, whether the evening digest covers it, and its other choices. A meeting kept on this Mac answers not_found.",
    inputSchema: {
      type: "object",
      properties: { meeting_id: MEETING_ID },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "series", "--json", "--", text(input, "meeting_id", true)],
  },
  {
    name: "sona_series_template",
    title: "Choose a series' notes template",
    description: `Choose the notes template for a meeting's series: a built-in one, one of the user's own by id, or none to clear it. ${SETTINGS_STORE_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        meeting_id: MEETING_ID,
        template: {
          type: "string",
          enum: ["general", "one-on-one", "interview", "sales-call", "standup", "none"],
          description: "A built-in template, or none to clear the choice.",
        },
        custom: { type: "string", description: "One of the user's own templates, by id, instead of template." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("series-template", input, agent),
      ...assigned("--custom", text(input, "custom")),
      "--",
      text(input, "meeting_id", true),
      ...(text(input, "template") === null ? [] : [text(input, "template", true)]),
    ],
  },
  {
    name: "sona_series_digest",
    title: "Keep a series in the evening digest",
    description: `Keep a meeting's series in the evening digest, or take it out with off. ${SETTINGS_STORE_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        meeting_id: MEETING_ID,
        off: { type: "boolean", description: "Take the series out of the evening digest." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["meeting_id"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("series-digest", input, agent),
      ...(flag(input, "off") ? ["--off"] : []),
      "--",
      text(input, "meeting_id", true),
    ],
  },
  {
    name: "sona_evening",
    title: "Read today's evening card",
    description:
      "Today's evening card: the lines Sona wrote about the day, and recordings waiting to be filed, each with suggestions of where it may belong. Lines drawn on a meeting kept on this Mac are left out, and lines_left_out says so.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "evening", "--json"],
  },
  {
    name: "sona_filing",
    title: "File a recording from the evening card",
    description: `File a recording where one suggestion from sona_evening says, or turn the suggestion down for good with decline. ${SETTINGS_STORE_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        suggestion: { type: "string", description: "The suggestion's id, from sona_evening." },
        decline: { type: "boolean", description: "Turn the suggestion down for good." },
        dry_run: DRY_RUN,
        operation_id: SETTINGS_OPERATION_ID,
      },
      required: ["suggestion"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      ...settingsWrite("filing", input, agent),
      ...(flag(input, "decline") ? ["--decline"] : []),
      "--",
      text(input, "suggestion", true),
    ],
  },
  {
    name: "sona_connections",
    title: "Read Sona's connections",
    description:
      "The connections (Slack, Notion, CRMs, webhooks) with their automatic-send rules and recent sends. Never a token. Sends of a meeting kept on this Mac are left out, and so are rules for a series kept on this Mac; rules_left_out says when one was.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "connections", "--json"],
  },
  {
    name: "sona_automation_list",
    title: "List Sona automations",
    description: `List automations: each one's name, id, prompt, whether it is on, its trigger, conditions and allowed tools, its version, next run and last run. ${AGENT_LOOP_KIND}`,
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    argv: () => ["work", "automation-list", "--json"],
  },
  {
    name: "sona_automation_show",
    title: "Show a Sona automation",
    description: `Show one automation with its saved versions and its recent runs, newest first: each run's cause, status (queued, running, done, failed or skipped), times, output or error, and any tools it was refused. ${AGENT_LOOP_KIND}`,
    inputSchema: {
      type: "object",
      properties: { automation: AGENT_LOOP },
      required: ["automation"],
      additionalProperties: false,
    },
    argv: (input) => ["work", "automation-show", "--json", "--", text(input, "automation", true)],
  },
  {
    name: "sona_automation_save",
    title: "Save a Sona automation",
    description: `Create an automation, or change one: saving a name that exists changes that automation; automation renames another to name. A new automation needs prompt and a trigger, either schedule with first_run or on. Fields left out keep what the automation had; each save is kept as a new version. ${AGENT_LOOP_KIND} ${AGENT_LOOP_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "The automation's name, 1 to 64 characters, unique ignoring case." },
        automation: { type: "string", description: "The automation to rename to name, by name or id." },
        prompt: { type: "string", description: "What the agent does when the automation runs, 1 to 4000 characters." },
        schedule: {
          type: "string",
          enum: ["hourly", "daily", "weekly", "monthly"],
          description: "Run on a schedule. Needs first_run; leave on out.",
        },
        first_run: {
          type: "string",
          description: "The first scheduled run, YYYY-MM-DD HH:MM in timezone. It sets the time of day, and the day of month for monthly, and must not be in the past.",
        },
        timezone: {
          type: "string",
          description: "The schedule's IANA time zone, such as America/Los_Angeles. Left out, this Mac's.",
        },
        days: {
          type: "array",
          items: { type: "string" },
          description: "The weekdays a weekly schedule runs on: mon, tue, wed, thu, fri, sat, sun.",
        },
        on: {
          type: "string",
          enum: [
            "task-created",
            "status-changed",
            "label-added",
            "assigned",
            "comment-added",
            "project-status-changed",
            "meeting-ended",
          ],
          description: "Run when this happens instead of on a schedule. assigned is a task getting an owner or a delegate; meeting-ended fires once per meeting, when its notes are ready.",
        },
        to: {
          type: "string",
          description: "With status-changed, only a change to this task status; with project-status-changed, to this project status. Left out, any change.",
        },
        label: {
          type: "string",
          description: "With label-added, only this label, by name or id. Left out, any label.",
        },
        if_label: {
          type: "array",
          items: { type: "string" },
          description: "Run only for tasks with one of these labels. Task events only. An empty list clears it.",
        },
        if_project: {
          type: "array",
          items: { type: "string" },
          description: "Run only for tasks in, or projects that are, one of these projects. Task and project events only. An empty list clears it.",
        },
        if_status: {
          type: "array",
          items: { type: "string" },
          description: "Run only for tasks in one of these statuses. Task events only. An empty list clears it. At most 8 conditions across the three lists.",
        },
        tools: {
          type: "array",
          items: { type: "string" },
          description: "The read-only Sona tool names the agent may use on a run, such as sona_search or sona_meeting; Sona refuses names it does not offer. An empty list clears it.",
        },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["name"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "automation-save",
      ...assigned("--automation", text(input, "automation")),
      ...assigned("--prompt", text(input, "prompt")),
      ...assigned("--schedule", text(input, "schedule")),
      ...assigned("--first-run", text(input, "first_run")),
      ...assigned("--timezone", text(input, "timezone")),
      ...joined("--days", input, "days"),
      ...assigned("--on", text(input, "on")),
      ...assigned("--to", text(input, "to")),
      ...assigned("--label", text(input, "label")),
      ...joined("--if-label", input, "if_label"),
      ...joined("--if-project", input, "if_project"),
      ...joined("--if-status", input, "if_status"),
      ...joined("--tools", input, "tools"),
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "name", true),
    ],
  },
  onAgentLoop(
    "run",
    "Run a Sona automation now",
    "Queue a run of an automation now and get back the queued run; the running Sona app starts it within seconds. Follow it with sona_automation_show. A run is skipped, with the reason, while External access or External mutations is off.",
    { mutates: "changes", optIn: true },
  ),
  onAgentLoop("enable", "Turn a Sona automation on", "Turn an automation on, so its trigger runs it again.", {
    mutates: "changes",
  }),
  onAgentLoop("disable", "Turn a Sona automation off", "Turn an automation off: its trigger stops running it until it is turned on.", {
    mutates: "changes",
  }),
  {
    name: "sona_automation_restore_version",
    title: "Restore a Sona automation version",
    description: `Bring back an earlier version of an automation: its name, prompt, trigger, conditions and tools. Restoring saves a new version with the old content. ${AGENT_LOOP_KIND} ${AGENT_LOOP_WRITES}`,
    mutates: "changes",
    inputSchema: {
      type: "object",
      properties: {
        automation: AGENT_LOOP,
        version: { type: "integer", minimum: 1, description: "The version number, from sona_automation_show." },
        dry_run: DRY_RUN,
        operation_id: OPERATION_ID,
      },
      required: ["automation", "version"],
      additionalProperties: false,
    },
    argv: (input, agent) => [
      "work",
      "automation-restore-version",
      ...(flag(input, "dry_run") ? ["--dry-run"] : []),
      ...writing(input, agent),
      "--json",
      "--",
      text(input, "automation", true),
      requiredCount(input, "version"),
    ],
  },
  onAgentLoop(
    "delete",
    "Delete a Sona automation",
    "Delete an automation for good, with its versions and runs.",
    { mutates: "destroys", optIn: true },
  ),
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
