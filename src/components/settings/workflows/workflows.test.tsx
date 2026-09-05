import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { WorkflowRunReceipt, WorkflowsListResult } from "@/bindings";
import { TooltipProvider } from "@/components/vg/tooltip";
import { WorkflowList } from "./WorkflowList";
import { WorkflowRunLog } from "./WorkflowRunLog";
import { runsForLastSevenDays } from "./workflowRuns";
import { formatWorkflowOutcome } from "./formatWorkflowOutcome";

const localePath = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);
const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localePath, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const nowMs = new Date(2026, 7, 30, 12, 0, 0).getTime();

const receipt = (
  id: string,
  status: WorkflowRunReceipt["status"],
  outcome: string,
  daysAgo: number,
): WorkflowRunReceipt => ({
  id,
  workflow_id:
    status === "failed"
      ? "continuity"
      : status === "skipped"
        ? "document_linking"
        : "person_linking",
  event_kind: "meeting_finalized",
  jump_target: null,
  status,
  started_at_utc_ms: nowMs - daysAgo * 86_400_000,
  finished_at_utc_ms: nowMs - daysAgo * 86_400_000 + 100,
  outcome_summary: outcome,
  outcome_code:
    status === "failed"
      ? "failed"
      : status === "skipped"
        ? "skipped"
        : "person_links",
  outcome_counts: {
    changes: status === "ok" ? 2 : 0,
    persons: 0,
    series: 0,
    carried: 0,
    candidates: 0,
    suggestions: 0,
    terms: 0,
    meetings: 0,
    loops_closed: 0,
    suggestions_waiting: 0,
    waiting_on_stale: 0,
  },
  error: status === "failed" ? "failed for test" : null,
});

describe("Workflows settings", () => {
  const data: WorkflowsListResult = {
    schema_version: 1,
    revision: 7,
    entries: [
      {
        id: "person_linking",
        enabled: true,
        last_run: receipt("latest", "ok", "Linked two people", 0),
      },
      { id: "vocabulary_mining", enabled: false, last_run: null },
    ],
  };

  test("renders each built-in with its description, switch and last receipt", () => {
    const markup = render(
      <WorkflowList
        data={data}
        loading={false}
        error={false}
        pendingWorkflowId={null}
        onRetry={() => {}}
        onToggle={() => {}}
        nowMs={nowMs}
      />,
    );

    /* The label names the outcome, not the subsystem: "Person linking" was a
     * module name that happened to be printed at a user. */
    expect(markup).toContain("Remember people");
    expect(markup).not.toContain("Person linking");
    expect(markup).toContain(
      "Connects meetings to people using confirmed attendee and speaker evidence.",
    );
    expect(markup).toContain("Remembered 2 people");
    expect(markup.includes("Updated 2 person links")).toBe(false);
    expect(markup).toContain("Completed");
    expect(markup).toContain("Not run yet");
    expect(markup).toContain('aria-label="Enable Remember people"');
    expect(markup).toContain('data-state="checked"');
    expect(markup).toContain('data-state="unchecked"');
  });

  test("renders the paged run log newest first with statuses and a seven-day chart", () => {
    const receipts = [
      receipt("newest", "ok", "Newest outcome", 0),
      receipt("failed", "failed", "Failed outcome", 1),
      receipt("skipped", "skipped", "Skipped outcome", 2),
    ];
    const markup = render(
      <WorkflowRunLog
        receipts={receipts}
        loading={false}
        loadingMore={false}
        error={false}
        hasMore={true}
        onRetry={() => {}}
        onLoadMore={() => {}}
        nowMs={nowMs}
      />,
    );

    expect(markup.indexOf('data-workflow-run-id="newest"')).toBeLessThan(
      markup.indexOf('data-workflow-run-id="failed"'),
    );
    expect(markup.indexOf('data-workflow-run-id="failed"')).toBeLessThan(
      markup.indexOf('data-workflow-run-id="skipped"'),
    );
    expect(markup).toContain("Remembered 2 people");
    expect(markup.includes("Newest outcome")).toBe(false);
    expect(markup).toContain("Completed");
    expect(markup).toContain("Failed");
    expect(markup).toContain("Skipped");
    expect(markup).toContain(
      'aria-label="Activity per day, 3 in the last 7 days"',
    );
    expect(markup).toContain("Show more");
    expect(markup.split("<rect").length - 1).toBe(7);
  });

  /* `settings.workflows.items` covers six of the eleven workflow ids; the five
   * learning loops live under `learningV2.workflows`. A run log that built its
   * copy key by template literal printed the key itself for those five, and
   * i18next is configured with no missing-key handler to catch it. */
  test("names a learning workflow from the catalogue, never by raw key", () => {
    const markup = render(
      <WorkflowRunLog
        receipts={[
          {
            ...receipt("mined", "ok", "spoken_punctuation:suggestions=1", 0),
            workflow_id: "spoken_punctuation",
            event_kind: "dictation_corpus_swept",
            outcome_code: "learning_suggestions",
            outcome_counts: {
              ...receipt("mined", "ok", "", 0).outcome_counts,
              changes: 0,
              suggestions: 1,
            },
          },
        ]}
        loading={false}
        loadingMore={false}
        error={false}
        hasMore={false}
        onRetry={() => {}}
        onLoadMore={() => {}}
        nowMs={nowMs}
      />,
    );

    expect(markup).toContain(
      i18n.t("learningV2.workflows.spokenPunctuation.name"),
    );
    expect(markup).not.toContain("settings.workflows.items");
    expect(markup).not.toContain("learningV2.workflows");
  });

  test("uses one quiet glyph and no chart before the first run", () => {
    const markup = render(
      <WorkflowRunLog
        receipts={[]}
        loading={false}
        loadingMore={false}
        error={false}
        hasMore={false}
        onRetry={() => {}}
        onLoadMore={() => {}}
        nowMs={nowMs}
      />,
    );

    expect(markup).toContain("No activity yet.");
    expect(markup.split("<svg").length - 1).toBe(1);
    expect(markup.includes("Activity per day")).toBe(false);
  });
});

describe("formatWorkflowOutcome", () => {
  const format = (
    code: WorkflowRunReceipt["outcome_code"],
    counts: Partial<WorkflowRunReceipt["outcome_counts"]> = {},
  ) => {
    const base = receipt("formatter", "ok", "machine-only", 0);
    return formatWorkflowOutcome(
      {
        ...base,
        outcome_code: code,
        outcome_counts: { ...base.outcome_counts, ...counts },
      },
      i18n.t,
    );
  };

  /* The sentences a person would say, counted from the receipt. The workflow's
   * own name never appears in one: "Person linking" is the subsystem talking
   * about itself, and no reader is owed that word. */
  test("maps every structured count family without using the stored summary", () => {
    expect(format("person_links", { changes: 1 })).toBe("Remembered 1 person");
    expect(format("person_links", { changes: 2 })).toBe("Remembered 2 people");
    expect(format("briefing", { persons: 2 })).toBe(
      "Prepared your meeting brief",
    );
    expect(format("continuity", { series: 3, carried: 2 })).toBe(
      "Carried 2 open loops forward",
    );
    expect(format("vocabulary_candidates", { candidates: 1 })).toBe(
      "Learned a new word",
    );
    expect(format("vocabulary_candidates", { candidates: 4 })).toBe(
      "Learned 4 new words",
    );
    expect(format("document_links", { changes: 1 })).toBe("Linked a document");
    expect(format("document_links", { changes: 2 })).toBe("Linked 2 documents");
    expect(format("already_processed")).toBe("Nothing new to do");
    expect(format("failed")).toBe("Couldn't finish");
    expect(format("skipped")).toBe("Skipped");
  });

  /* The consent popup writes its own history into the same feed. Each line
   * says what happened to the recording; "prompt", "consent" and "receipt"
   * are words for the session manager, not for a reader. */
  test("narrates what the consent popup did to the recording", () => {
    expect(format("prompt_recorded")).toBe("Started recording");
    expect(format("prompt_ignored")).toBe(
      "Skipped recording a detected meeting",
    );
    expect(format("auto_record_started")).toBe(
      "Started recording automatically",
    );
    expect(format("auto_record_stopped")).toBe(
      "Stopped recording automatically",
    );
  });

  test("narrates each meeting ritual receipt", () => {
    expect(format("prep_presented")).toBe("Prep");
    expect(format("prep_record_armed")).toBe("Record when it starts");
    expect(format("prep_brief_opened")).toBe("Open brief");
    expect(format("prep_dismissed")).toBe("Dismiss");
    expect(format("wrap_presented")).toBe("Wrap");
    expect(format("wrap_notes_opened")).toBe("Open notes");
    expect(format("wrap_follow_up_copied")).toBe("Copied");
    expect(format("wrap_done")).toBe("Done");
  });
});

describe("runsForLastSevenDays", () => {
  test("uses local calendar-day buckets and ignores older receipts", () => {
    expect(
      runsForLastSevenDays(
        [
          receipt("today", "ok", "", 0),
          receipt("yesterday", "ok", "", 1),
          receipt("also-yesterday", "ok", "", 1),
          receipt("too-old", "ok", "", 7),
        ],
        nowMs,
      ),
    ).toEqual([0, 0, 0, 0, 0, 2, 1]);
  });
});
