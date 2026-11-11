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
import {
  SonaInputError,
  TOOLS,
  type ToolDefinition,
  type ToolInput,
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

  test("a task save sends its steps in order as one JSON argument", () => {
    const save = tool("sona_task_save");
    const patch: Record<string, string | string[] | null>[] = [
      { field: "status", value: "in_progress" },
      { field: "labels", op: "add", value: ["bug"] },
      { field: "due", value: null },
    ];

    expect(save.argv({ task: "S-1", patch, expected_revision: 3 }, "Claude")).toEqual([
      "work",
      "save",
      `--patch-json=${JSON.stringify(patch)}`,
      "--expected-revision=3",
      "--agent=Claude",
      "--json",
      "--",
      "S-1",
    ]);
    expect(() => save.argv({ task: "S-1" })).toThrow("patch is required");
    expect(() => save.argv({ task: "S-1", patch: ["status"] })).toThrow(SonaInputError);
  });

  test("a session question carries its choices and the plan as JSON", () => {
    const options = [{ label: "Ship it", value: "ship" }];
    const plan = [{ content: "Read the spec", status: "completed" }];

    expect(
      tool("sona_session_activity").argv(
        { session: "5b0c6a8e-0000-4000-8000-000000000001", type: "elicitation", body: "Ship?", options, plan },
        "Claude",
      ),
    ).toEqual([
      "work",
      "session-activity",
      "--type=elicitation",
      "--body=Ship?",
      `--options-json=${JSON.stringify(options)}`,
      `--plan-json=${JSON.stringify(plan)}`,
      "--agent=Claude",
      "--json",
      "--",
      "5b0c6a8e-0000-4000-8000-000000000001",
    ]);
  });

  test("person stats are read by the exact person id", () => {
    const stats = tool("sona_person_stats");

    expect(stats.argv({ person_id: "2e2a5f0e-0000-4000-8000-000000000002" })).toEqual([
      "--person-stats",
      "2e2a5f0e-0000-4000-8000-000000000002",
    ]);
    expect(() => stats.argv({})).toThrow(SonaInputError);
  });

  test("a comment save without id adds one, with id edits the one named", () => {
    const save = tool("sona_comment_save");
    const comment = "e7f77ae7-0000-4000-8000-000000000001";

    expect(save.argv({ task: "S-1", body: "-started", reply_to: comment }, "Claude")).toEqual([
      "work",
      "comment",
      "add",
      "--body=-started",
      `--reply-to=${comment}`,
      "--agent=Claude",
      "--json",
      "--",
      "S-1",
    ]);
    expect(save.argv({ id: comment, body: "fixed", dry_run: true })).toEqual([
      "work",
      "comment-edit",
      "--body=fixed",
      "--dry-run",
      "--json",
      "--",
      comment,
    ]);
    expect(() => save.argv({ id: comment, task: "S-1", body: "x" })).toThrow(SonaInputError);
    expect(() => save.argv({ body: "x" })).toThrow("task is required");
  });

  test("a null parent lifts a task to top level", () => {
    const update = tool("sona_work_update");

    expect(update.argv({ tasks: "S-4", parent: null })).toContain("--parent=none");
    expect(update.argv({ tasks: "S-4", parent: "S-1" })).toContain("--parent=S-1");
    expect(update.argv({ tasks: "S-4" }).some((arg) => arg.startsWith("--parent"))).toBe(false);
  });

  test("planner and note tools spell their choices as the CLI reads them", () => {
    const note = "5b0c6a8e-0000-4000-8000-000000000003";
    const subject = "5b0c6a8e-0000-4000-8000-000000000004";
    const placed = tool("sona_scratchpad_subject_note");

    expect(placed.argv({ subject_id: subject, note_id: note })).toEqual([
      "work",
      "subject-note",
      "--json",
      "--",
      subject,
      note,
    ]);
    expect(placed.argv({ subject_id: subject, note_id: note, action: "current" })).toContain("--current");
    expect(() => placed.argv({ subject_id: subject, note_id: note, action: "drop" })).toThrow(SonaInputError);
    expect(() => tool("sona_work_watch_task").argv({ watch: "pricing" })).toThrow("line is required");
    expect(
      tool("sona_work_view_save").argv({ name: "Mine", show_empty_groups: false, properties: ["id", "due"] }),
    ).toEqual(expect.arrayContaining(["--show-empty-groups=false", "--properties=id,due"]));
    expect(tool("sona_scratchpad_delete").mutates).toBe("destroys");
  });

  test("a setting's value arrives whole after --, and settings that take things away are opt-in", () => {
    expect(tool("sona_setting").argv({ key: "keyword-trackers", value: "-[]", dry_run: true }, "Claude")).toEqual([
      "work",
      "setting",
      "--dry-run",
      "--agent=Claude",
      "--json",
      "--",
      "keyword-trackers",
      "-[]",
    ]);
    expect(() => tool("sona_setting").argv({ key: "theme", value: true })).toThrow(SonaInputError);
    expect(tool("sona_mode_save").argv({ mode: '{"id":"m","name":"-x"}' })).toContain(
      '--mode-json={"id":"m","name":"-x"}',
    );
    for (const name of ["sona_setting_reset", "sona_mode_delete", "sona_prompt_delete"]) {
      expect(tool(name).mutates).toBe("destroys");
      expect(tool(name).optIn).toBe(true);
    }
  });

  test("a meeting write keeps a dash-led title a value, and meeting removals are opt-in", () => {
    const meeting = "5b0c6a8e-0000-4000-8000-000000000001";
    expect(tool("sona_meeting_rename").argv({ meeting_id: meeting, title: "-Pricing", dry_run: true }, "Claude")).toEqual([
      "work",
      "meeting-rename",
      "--dry-run",
      "--agent=Claude",
      "--json",
      "--",
      meeting,
      "-Pricing",
    ]);
    expect(tool("sona_meeting_unfile").argv({ meeting_id: meeting, folder: "-Q3" }).slice(0, 3)).toEqual([
      "work",
      "folder-remove",
      "--folder=-Q3",
    ]);
    expect(tool("sona_heard_plan_edit").argv({ plan_id: "p", people: [] })).toContain("--people=");
    expect(tool("sona_heard_plan_edit").argv({ plan_id: "p" }).some((arg) => arg.startsWith("--people"))).toBe(false);
    for (const name of [
      "sona_meeting_delete",
      "sona_meeting_folder_delete",
      "sona_meeting_note_delete",
      "sona_meeting_my_notes",
      "sona_meeting_snapshot_delete",
      "sona_meeting_delete_forever",
    ]) {
      expect(tool(name).mutates).toBe("destroys");
      expect(tool(name).optIn).toBe(true);
    }
    expect(tool("sona_meeting_restore").argv({ trash_id: "-entry" }, "Claude").slice(-2)).toEqual(["--", "-entry"]);
    expect(tool("sona_meeting_restore").mutates).toBe("changes");
    expect(tool("sona_meeting_trash").argv({})).toEqual(["work", "meeting-trash", "--json", "--"]);
  });

  test("transcript and dictation writes keep dash-led words values, and the ones that delete are opt-in", () => {
    const meeting = "5b0c6a8e-0000-4000-8000-000000000001";
    expect(tool("sona_transcript_line_edit").argv({ meeting_id: meeting, line_id: "l", text: "-ok", dry_run: true }, "Claude")).toEqual([
      "work",
      "line-edit",
      "--dry-run",
      "--agent=Claude",
      "--json",
      "--",
      meeting,
      "l",
      "-ok",
    ]);
    expect(tool("sona_speaker_merge").argv({ meeting_id: meeting, speaker: "Speaker 2", into: "-Dana" }).slice(0, 3)).toEqual([
      "work",
      "speaker-merge",
      "--into=-Dana",
    ]);
    expect(tool("sona_transcript_correction_edit").argv({ person: "Dana", correction_id: "c", enabled: false })).toContain("--pause");
    expect(tool("sona_dictation_save").argv({ dictation_id: 7, saved: false })).toEqual([
      "work",
      "dictation-save",
      "--unsave",
      "--json",
      "--",
      "7",
    ]);
    expect(tool("sona_dictation_save").argv({ dictation_id: 7 })).not.toContain("--unsave");
    expect(() => tool("sona_dictation_delete").argv({ dictation_id: "7" })).toThrow(SonaInputError);
    for (const name of ["sona_speaker_merge", "sona_transcript_correction_remove", "sona_dictation_delete"]) {
      expect(tool(name).mutates).toBe("destroys");
      expect(tool(name).optIn).toBe(true);
    }
    expect(tool("sona_speaker_rename").mutates).toBe("changes");
  });

  test("an attach takes a path or a url, never both, as the last argument", () => {
    const attach = tool("sona_task_attach");

    expect(
      attach.argv({ task: "S-1", path: "/tmp/-report.md", title: "-Report", dry_run: true }, "Claude"),
    ).toEqual([
      "work",
      "attach",
      "--title=-Report",
      "--dry-run",
      "--agent=Claude",
      "--json",
      "--",
      "S-1",
      "/tmp/-report.md",
    ]);
    expect(attach.argv({ task: "S-1", url: "https://example.com/pr/1" }).at(-1)).toBe(
      "https://example.com/pr/1",
    );
    expect(() => attach.argv({ task: "S-1" })).toThrow("exactly one of path or url");
    expect(() =>
      attach.argv({ task: "S-1", path: "/tmp/a", url: "https://example.com" }),
    ).toThrow("exactly one of path or url");
  });

  test("a speaker assign names the meeting and speaker last, and the person by flag", () => {
    const assign = tool("sona_speaker_assign");
    const meeting = "5b0c6a8e-0000-4000-8000-000000000001";

    expect(
      assign.argv({ meeting_id: meeting, speaker: "Speaker 2", person: "-Dana", dry_run: true }, "Claude"),
    ).toEqual([
      "work",
      "speaker-assign",
      "--person=-Dana",
      "--dry-run",
      "--agent=Claude",
      "--json",
      "--",
      meeting,
      "Speaker 2",
    ]);
    expect(assign.argv({ meeting_id: meeting, speaker: "Speaker 2", new_person: "Lee Park" })).toContain(
      "--new=Lee Park",
    );
    expect(assign.argv({ meeting_id: meeting, speaker: "Speaker 2", undo: true })).toContain("--undo");
  });

  test("people and voice tools name the person or meeting last, after --", () => {
    const meeting = "5b0c6a8e-0000-4000-8000-000000000001";

    expect(tool("sona_person_link").argv({ person: "-Dana", meeting_id: meeting, confirm: true }, "Claude")).toEqual([
      "work",
      "person-link",
      "--confirm",
      "--agent=Claude",
      "--json",
      "--",
      "-Dana",
      meeting,
    ]);
    expect(
      tool("sona_person_split").argv({ person: "Dana", new_person: "-Dana Two", meetings: `${meeting},x` }),
    ).toEqual(["work", "person-split", "--new=-Dana Two", `--meetings=${meeting},x`, "--json", "--", "Dana"]);
    expect(tool("sona_person_first_met_answer").argv({ person: "Dana", answer: "no" }).slice(-2)).toEqual([
      "Dana",
      "no",
    ]);
    expect(tool("sona_person_merge").argv({ person: "A", into: "B", voice: "both" })).toContain("--voice=both");
    expect(tool("sona_person_merge").argv({ person: "A", into: "B", dry_run: true })).toContain("--dry-run");
    expect(
      tool("sona_speaker_assign").argv({ meeting_id: meeting, speaker: "Speaker 2", person: "Dana", same_voice: true }),
    ).toContain("--same-voice");
    expect(tool("sona_speaker_voice_undo").argv({ meeting_id: meeting, speaker: "-Lee" }).slice(-3)).toEqual([
      "--",
      meeting,
      "-Lee",
    ]);
  });

  test("a source link names its kind as the flag and the target last", () => {
    const link = tool("sona_planning_link_source");
    const meeting = "5b0c6a8e-0000-4000-8000-000000000001";

    expect(
      link.argv({ target: "-Launch", kind: "meeting", source_id: meeting, at_seconds: 65, dry_run: true }, "Claude"),
    ).toEqual([
      "work",
      "source-link",
      `--meeting=${meeting}`,
      "--at=65",
      "--dry-run",
      "--agent=Claude",
      "--json",
      "--",
      "-Launch",
    ]);
    expect(() => link.argv({ target: "S-1", kind: "folder", source_id: meeting })).toThrow(
      "kind must be meeting, note or loop",
    );
  });

  test("an update save without id posts one, with id edits the one named", () => {
    const save = tool("sona_project_update_save");
    const update = "06d5d03f-0000-4000-8000-000000000001";

    expect(
      save.argv({ project: "-Harbor", health: "at-risk", body: "-Venue held." }, "Claude"),
    ).toEqual([
      "work",
      "project-update",
      "--health=at-risk",
      "--body=-Venue held.",
      "--agent=Claude",
      "--json",
      "--",
      "-Harbor",
    ]);
    expect(save.argv({ project: "Harbor", health: "on-track", draft: true, dry_run: true })).toEqual([
      "work",
      "project-update",
      "--health=on-track",
      "--dry-run",
      "--json",
      "--",
      "Harbor",
    ]);
    expect(save.argv({ id: update, health: "off-track" })).toEqual([
      "work",
      "project-update-edit",
      "--health=off-track",
      "--json",
      "--",
      update,
    ]);
    expect(() => save.argv({ project: "Harbor", health: "on-track" })).toThrow(
      "exactly one of body or draft",
    );
    expect(() =>
      save.argv({ project: "Harbor", health: "on-track", body: "x", draft: true }),
    ).toThrow("exactly one of body or draft");
    expect(() => save.argv({ id: update, project: "Harbor" })).toThrow(SonaInputError);
  });

  test("a favorite names its kind, and an inbox mark of all names no ids", () => {
    const favorite = tool("sona_work_favorite");
    expect(favorite.argv({ kind: "task", target: "-S-1" }, "Claude")).toEqual([
      "work",
      "favorite",
      "--agent=Claude",
      "--json",
      "--",
      "-S-1",
    ]);
    expect(favorite.argv({ kind: "view", target: "-Open work", dry_run: true })).toEqual([
      "work",
      "favorite",
      "--dry-run",
      "--json",
      "--view=-Open work",
    ]);
    expect(() => favorite.argv({ kind: "label", target: "ops" })).toThrow(SonaInputError);

    const mark = tool("sona_work_inbox_mark");
    expect(mark.argv({ notifications: "All", mark: "read" })).toEqual([
      "work",
      "inbox-mark",
      "--mark=read",
      "--all",
      "--json",
    ]);
    expect(mark.argv({ notifications: "a, b", mark: "snooze", until: "tomorrow" })).toEqual([
      "work",
      "inbox-mark",
      "--mark=snooze",
      "--until=tomorrow",
      "--json",
      "--",
      "a",
      "b",
    ]);
  });

  test("a project save clears each planning field with null", () => {
    const argv = tool("sona_project_save").argv({
      name: "Harbor",
      priority: null,
      start: null,
      start_resolution: null,
      target_resolution: "quarter",
      icon: null,
      labels: null,
      links: null,
    });

    expect(argv).toEqual([
      "work",
      "project-save",
      "--priority=none",
      "--start=none",
      "--start-resolution=none",
      "--target-resolution=quarter",
      "--icon=none",
      "--labels=",
      "--link=none",
      "--json",
      "--",
      "Harbor",
    ]);
    expect(
      tool("sona_project_save").argv({ name: "Harbor", links: ["Spec=https://a.example", "https://b.example"] }),
    ).toContain("--link=https://b.example");
  });

  test("a project save passes me through as lead and member", () => {
    const argv = tool("sona_project_save").argv({ name: "Harbor", lead: "me", members: "me,Dana" });

    expect(argv).toContain("--lead=me");
    expect(argv).toContain("--members=me,Dana");
  });

  test("a cycle save runs the one verb its arguments name", () => {
    const save = tool("sona_cycle_save");

    expect(save.argv({ weeks: 3, cooldown_weeks: 1, upcoming: 3, dry_run: true })).toEqual([
      "work",
      "cycle-settings",
      "--weeks=3",
      "--cooldown=1",
      "--upcoming=3",
      "--dry-run",
      "--json",
    ]);
    expect(save.argv({ cycle: "next", name: null })).toEqual([
      "work",
      "cycle-save",
      "--name=none",
      "--json",
      "--",
      "next",
    ]);
    expect(save.argv({ start_today: true })).toEqual(["work", "cycle-start-today", "--json"]);
    expect(save.argv({ cycle: "2", rollover: true }).slice(1, 2)).toEqual(["cycle-rollover"]);
    expect(() => save.argv({ cycle: "2", rollover: true, name: "Push" })).toThrow(
      "one kind of change",
    );
    expect(() => save.argv({ cycle: "2", weeks: 2 })).toThrow("leave cycle out");
    expect(() => save.argv({ rollover: true })).toThrow("cycle is required");
  });

  test("an automation save spells its trigger, conditions and tools as single flags", () => {
    const save = tool("sona_automation_save");

    expect(
      save.argv(
        {
          name: "-Weekly digest",
          prompt: "Sum up the week",
          schedule: "weekly",
          first_run: "2026-11-16 09:00",
          timezone: "America/Los_Angeles",
          days: ["mon", "thu"],
          tools: ["sona_search", "sona_meeting"],
          dry_run: true,
        },
        "Claude",
      ),
    ).toEqual([
      "work",
      "automation-save",
      "--prompt=Sum up the week",
      "--schedule=weekly",
      "--first-run=2026-11-16 09:00",
      "--timezone=America/Los_Angeles",
      "--days=mon,thu",
      "--tools=sona_search,sona_meeting",
      "--dry-run",
      "--agent=Claude",
      "--json",
      "--",
      "-Weekly digest",
    ]);
    expect(
      save.argv({
        name: "Triage bugs",
        automation: "Old name",
        on: "status-changed",
        to: "in_progress",
        if_label: ["bug", "ops"],
        if_project: ["Harbor"],
        if_status: [],
        tools: [],
      }),
    ).toEqual([
      "work",
      "automation-save",
      "--automation=Old name",
      "--on=status-changed",
      "--to=in_progress",
      "--if-label=bug,ops",
      "--if-project=Harbor",
      "--if-status=",
      "--tools=",
      "--json",
      "--",
      "Triage bugs",
    ]);
    expect(() => save.argv({ name: "x", days: "mon" })).toThrow("days must be a list of strings");
    expect(() => save.argv({ prompt: "x" })).toThrow("name is required");
  });

  test("an automation run, restore and delete name the automation last, and run and delete are opt-in", () => {
    expect(tool("sona_automation_run").argv({ automation: "-Digest" }, "Claude")).toEqual([
      "work",
      "automation-run",
      "--agent=Claude",
      "--json",
      "--",
      "-Digest",
    ]);
    expect(tool("sona_automation_restore_version").argv({ automation: "Digest", version: 3, dry_run: true })).toEqual([
      "work",
      "automation-restore-version",
      "--dry-run",
      "--json",
      "--",
      "Digest",
      "3",
    ]);
    expect(() => tool("sona_automation_restore_version").argv({ automation: "Digest" })).toThrow("version is required");
    expect(tool("sona_automation_disable").argv({ automation: "Digest" })).toEqual([
      "work",
      "automation-disable",
      "--json",
      "--",
      "Digest",
    ]);
    expect(tool("sona_automation_delete").argv({ automation: "Digest", operation_id: "op-1" })).toEqual([
      "work",
      "automation-delete",
      "--operation-id=op-1",
      "--json",
      "--",
      "Digest",
    ]);
    expect(tool("sona_automation_delete").mutates).toBe("destroys");
    expect(tool("sona_automation_delete").optIn).toBe(true);
    expect(tool("sona_automation_run").optIn).toBe(true);
    expect(tool("sona_automation_save").optIn).toBeUndefined();
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

async function connected(selection?: string): Promise<Client> {
  const [clientEnd, serverEnd] = InMemoryTransport.createLinkedPair();
  const client = new Client({ name: "sona-mcp-test", version: "0.1.0" });
  await Promise.all([
    createServer(selection).connect(serverEnd),
    client.connect(clientEnd),
  ]);
  return client;
}

async function published(selection?: string) {
  const client = await connected(selection);
  try {
    return (await client.listTools()).tools;
  } finally {
    await client.close();
  }
}

/** The verbs and flags with which `sona` changes something. */
const SONA_WRITES = {
  "meeting-restore": true,
  "meeting-delete-forever": true,
  "meeting-export": true,
  "meeting-rename": true,
  "meeting-delete": true,
  "folder-new": true,
  "folder-rename": true,
  "folder-delete": true,
  "folder-add": true,
  "folder-remove": true,
  "meeting-note-add": true,
  "meeting-note-edit": true,
  "meeting-note-delete": true,
  "my-notes": true,
  "action-done": true,
  "snapshot-delete": true,
  "heard-plan-edit": true,
  "heard-plan-not-now": true,
  save: true,
  "session-start": true,
  "session-activity": true,
  "session-plan": true,
  "session-heartbeat": true,
  "session-stop": true,
  "--loop-resolve": true,
  create: true,
  update: true,
  claim: true,
  done: true,
  accept: true,
  decline: true,
  duplicate: true,
  archive: true,
  restore: true,
  relate: true,
  unrelate: true,
  "label-save": true,
  "label-delete": true,
  "label-group": true,
  "label-retire": true,
  "template-save": true,
  "template-delete": true,
  "template-use": true,
  "automation-save": true,
  "automation-run": true,
  "automation-enable": true,
  "automation-disable": true,
  "automation-restore-version": true,
  "automation-delete": true,
  remind: true,
  "view-save": true,
  "view-delete": true,
  say: true,
  watch: true,
  unwatch: true,
  "project-save": true,
  "project-delete": true,
  "project-update": true,
  "project-update-edit": true,
  "project-update-delete": true,
  "milestone-reorder": true,
  "cycle-settings": true,
  "cycle-save": true,
  "cycle-rollover": true,
  "cycle-start-today": true,
  "project-archive": true,
  "milestone-save": true,
  "milestone-delete": true,
  "person-save": true,
  "person-merge": true,
  comment: true,
  "comment-edit": true,
  "comment-delete": true,
  "comment-resolve": true,
  subtask: true,
  attach: true,
  delegate: true,
  focus: true,
  keep: true,
  "talking-point": true,
  "talking-point-done": true,
  "loop-reopen": true,
  "loop-owner": true,
  "action-track": true,
  "action-dismiss": true,
  "action-restore": true,
  "note-new": true,
  "note-edit": true,
  "note-pin": true,
  "note-link": true,
  "note-unlink": true,
  "note-delete": true,
  "note-restore": true,
  "subject-new": true,
  "subject-note": true,
  "book": true,
  "unbook": true,
  "nudged": true,
  "watch-task": true,
  "link-accept": true,
  "link-dismiss": true,
  "suggestion-accept": true,
  "suggestion-dismiss": true,
  "plan-accept": true,
  "attachment-rename": true,
  "speaker-assign": true,
  "speaker-confirm": true,
  "speaker-voice-undo": true,
  "person-link": true,
  "person-split": true,
  "person-delete": true,
  "person-photo": true,
  "person-contact-remove": true,
  "first-met-answer": true,
  "source-link": true,
  "doc-add": true,
  "doc-delete": true,
  detach: true,
  delete: true,
  "description-restore": true,
  favorite: true,
  unfavorite: true,
  subscribe: true,
  unsubscribe: true,
  "inbox-mark": true,
  setting: true,
  "setting-reset": true,
  "mode-save": true,
  "mode-delete": true,
  "mode-reorder": true,
  "prompt-save": true,
  "prompt-delete": true,
  "vocabulary-add": true,
  "vocabulary-csv": true,
  "workflow-enable": true,
  "saved-prompt-save": true,
  "saved-prompt-delete": true,
  "series-template": true,
  "series-digest": true,
  filing: true,
  "speaker-rename": true,
  "speaker-merge": true,
  "line-edit": true,
  "correction-add": true,
  "correction-edit": true,
  "correction-remove": true,
  "dictation-save": true,
  "dictation-delete": true,
};

/** The verbs with which `sona` takes away what no Sona tool gives back. */
const SONA_DESTROYS = {
  "meeting-delete-forever": true,
  "automation-delete": true,
  "meeting-delete": true,
  "folder-delete": true,
  "meeting-note-delete": true,
  "my-notes": true,
  "snapshot-delete": true,
  "label-delete": true,
  "view-delete": true,
  "milestone-delete": true,
  "project-delete": true,
  "project-update-delete": true,
  "person-merge": true,
  "comment-delete": true,
  detach: true,
  "template-delete": true,
  "speaker-assign": true,
  "person-split": true,
  "person-delete": true,
  "person-photo": true,
  "person-contact-remove": true,
  "doc-delete": true,
  "note-delete": true,
  "setting-reset": true,
  "mode-delete": true,
  "prompt-delete": true,
  "saved-prompt-delete": true,
  "speaker-merge": true,
  "correction-remove": true,
  "dictation-delete": true,
};

/** Each required argument at its first enum value or a placeholder, plus the
 * optional ones a tool needs one of: task, since a comment save needs task or
 * id; path, since an attach needs path or url; project, health and body,
 * since an update save needs project or id; cycle, since a cycle save needs
 * a cycle or settings. */
function smallest(tool: ToolDefinition): ToolInput {
  const input: ToolInput = {};
  const names = [...(tool.inputSchema.required ?? [])];
  for (const optional of ["task", "path", "project", "health", "body", "cycle"]) {
    if (optional in tool.inputSchema.properties && !names.includes(optional)) names.push(optional);
  }
  for (const name of names) {
    const property = tool.inputSchema.properties[name];
    input[name] =
      property?.enum?.[0] ??
      (property?.type === "integer"
        ? 1
        : property?.type === "boolean"
          ? true
          : property?.type === "array"
            ? property.items?.type === "object"
              ? [{}]
              : ["S-1"]
            : "S-1");
  }
  return input;
}

describe("published tools", () => {
  test("opt-in tools are published only when SONA_MCP_TOOLS names them", async () => {
    const names = async (selection?: string) =>
      (await published(selection)).map((tool) => tool.name).sort();

    const core = await names();

    expect(core).toContain("sona_work_update");
    expect(core).not.toContain("sona_work_archive");
    expect(await names("sona_work_archive")).toEqual(
      [...core, "sona_work_archive"].sort(),
    );
    expect(await names("all")).toHaveLength(TOOLS.length);
  });

  test("a SONA_MCP_TOOLS name that matches no tool stops the server", () => {
    expect(() => createServer("sona_work_lables")).toThrow(
      "SONA_MCP_TOOLS names no tool sona_work_lables",
    );
  });

  /* Clients skip the approval prompt for a tool published as read-only, so a
   * write tool missing `mutates` would change the user's tasks unasked. */
  test("every tool that runs a sona write is published as one", async () => {
    const listed = await published("all");

    const mislabelled = TOOLS.filter((tool) => {
      const argv = tool.argv(smallest(tool));
      const verb = (argv[0] === "work" ? argv[1] : argv[0]) ?? "";
      const writes = verb in SONA_WRITES || argv.some((arg) => arg.startsWith("--take="));
      const annotations = listed.find((candidate) => candidate.name === tool.name)
        ?.annotations;
      return (
        annotations?.readOnlyHint !== !writes ||
        annotations?.destructiveHint !== verb in SONA_DESTROYS
      );
    }).map((tool) => tool.name);

    expect(mislabelled).toEqual([]);
  });

  /* A read-only tool is published by default and skips the approval prompt,
   * so a file it wrote would be replaced without the user being asked. */
  test("no tool published as read-only writes a file", () => {
    const writers = TOOLS.filter(
      (tool) => tool.mutates === undefined && "out" in tool.inputSchema.properties,
    ).map((tool) => tool.name);

    expect(writers).toEqual([]);
  });
});

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

  test("sub-task titles arrive as one argument each", async () => {
    const sona = stub({ stdout: "{}" });
    const client = await connected();
    try {
      await client.callTool({
        name: "sona_subtask_create",
        arguments: { parent: "S-1", titles: ["Book the venue", "--order banners"] },
      });
    } finally {
      await client.close();
    }

    expect(sona.argv().slice(-4)).toEqual(["--", "S-1", "Book the venue", "--order banners"]);
  });

  test("filter conditions arrive as one JSON argument, and a malformed one is refused", async () => {
    const sona = stub({ stdout: "{}" });
    const filter = [
      { field: "label", op: "includes_all", values: ["bug", "ui"] },
      { field: "priority", op: "is", values: [1] },
    ];
    const client = await connected();
    try {
      await client.callTool({ name: "sona_work_list", arguments: { owner: "me", filter } });
    } finally {
      await client.close();
    }

    expect(sona.argv()).toContain(`--filter=${JSON.stringify(filter)}`);

    const malformed = await errorOf("sona_work_list", {
      filter: [{ field: "label", op: "is", values: ["bug"], extra: true }],
    });
    expect(malformed.code).toBe(ErrorCode.InvalidParams);
  });

  test("filter words and the timeline's days each arrive as one flag", async () => {
    const sona = stub({ stdout: "{}" });
    const client = await connected();
    try {
      await client.callTool({ name: "sona_work_list", arguments: { where: "--mine high priority bugs" } });
      expect(sona.argv()).toContain("--where=--mine high priority bugs");

      await client.callTool({ name: "sona_project_timeline", arguments: { from: "2026-11-01", to: "2027-01-31" } });
      expect(sona.argv().slice(-5)).toEqual(["work", "timeline", "--from=2026-11-01", "--to=2027-01-31", "--json"]);
    } finally {
      await client.close();
    }
  });

  test("a tool this server does not have is method-not-found", async () => {
    const failed = await errorOf("sona_delete_everything", {});

    expect(failed.code).toBe(ErrorCode.MethodNotFound);
    expect(failed.message).toContain("sona_delete_everything");
  });
});
