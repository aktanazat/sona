import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  LearningSuggestionEntry,
  PersonOpenLoop,
  UpdateCheckResult,
  WorkflowRunReceipt,
} from "@/bindings";
import { NeedsYou, Recent } from "./OverviewFeed";

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
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
    en: { translation: JSON.parse(fs.readFileSync(localeRoot, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const nowMs = new Date(2026, 7, 30, 12, 0, 0).getTime();

const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);

const occurrences = (markup: string, needle: string): number =>
  markup.split(needle).length - 1;

const minutesAgo = (minutes: number): number => nowMs - minutes * 60_000;

const NO_COUNTS = {
  changes: 0,
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
};

const linkedPeople: WorkflowRunReceipt = {
  id: "run-people",
  workflow_id: "person_linking",
  event_kind: "meeting_finalized",
  jump_target: { kind: "meeting", session_id: "meeting-a" },
  status: "ok",
  started_at_utc_ms: minutesAgo(41),
  finished_at_utc_ms: minutesAgo(40),
  outcome_summary: "person linking",
  outcome_code: "person_links",
  outcome_counts: { ...NO_COUNTS, changes: 2, persons: 2 },
  error: null,
};

/* A meeting-recording run with nothing to open: skipping a detected meeting
 * leaves no session, and the line still has to reach the reader. */
const skippedRecording: WorkflowRunReceipt = {
  ...linkedPeople,
  id: "run-skip",
  workflow_id: "meeting_activity",
  jump_target: null,
  finished_at_utc_ms: minutesAgo(8),
  outcome_code: "prompt_ignored",
  outcome_counts: NO_COUNTS,
};

/* Yesterday, so "2 today" cannot be read as "however many rows are showing". */
const oldRun: WorkflowRunReceipt = {
  ...linkedPeople,
  id: "run-old",
  workflow_id: "vocabulary_mining",
  jump_target: { kind: "meeting", session_id: "meeting-b" },
  finished_at_utc_ms: nowMs - 30 * 60 * 60_000,
  outcome_code: "vocabulary_candidates",
  outcome_counts: { ...NO_COUNTS, candidates: 3 },
};

/* A pass that changed nothing still writes a receipt. The run log under
 * Settings is where "Noticed 0 things" belongs. */
const quietRun: WorkflowRunReceipt = {
  ...linkedPeople,
  id: "run-quiet",
  workflow_id: "spoken_punctuation",
  outcome_code: "learning_suggestions",
  outcome_counts: NO_COUNTS,
};

const openLoop: PersonOpenLoop = {
  loop_id: "loop-open",
  meeting_id: "meeting-a",
  title: "Weekly sync",
  at_utc_ms: minutesAgo(40),
  text: "Send Priya the revised timeline",
  owner_person_id: null,
  status: "open",
  direction: "waiting_on",
  waiting_on_stale: false,
  carried_since_at_utc_ms: null,
  carried_into_meeting_id: null,
};

const suggestion: LearningSuggestionEntry = {
  loop_kind: "spoken_punctuation",
  candidate_key: "open paren",
  suggestion: {
    kind: "spoken_punctuation",
    spoken: "open paren",
    written: "(",
  },
  evidence: { occurrences: 7, distinct_days: 3, examples: [] },
  generated_at_utc_ms: nowMs,
};

const advice: LearningSuggestionEntry = {
  loop_kind: "capture_advice",
  candidate_key: "retry_rate:Zoom",
  suggestion: {
    kind: "capture_advice",
    advice: "retry_rate",
    subject: "Zoom",
    stat_permille: 2400,
    sample_runs: 40,
  },
  evidence: { occurrences: 12, distinct_days: 5, examples: [] },
  generated_at_utc_ms: nowMs,
};

const updateWaiting: UpdateCheckResult = {
  current_version: "1.0.0",
  latest_version: "1.1.0",
  update_available: true,
  url: "https://github.com/aktanazat/sona/releases/tag/v1.1.0",
  notes_excerpt: null,
  published_at_utc_ms: null,
  status: "update_available",
  error: null,
};

/* renderToStaticMarkup escapes an apostrophe, so the catalogue's sentence
 * arrives as an entity. Assert what a browser is actually handed. */
const LOAD_ERROR = "Couldn&#x27;t load this list.";

const needsYou = (
  overrides: Partial<React.ComponentProps<typeof NeedsYou>> = {},
): string =>
  render(
    <NeedsYou
      openLoops={{ status: "loaded", entries: [] }}
      learning={[]}
      update={null}
      updateChecked
      onAnswerLearning={() => {}}
      onDismissUpdate={() => {}}
      onOpenMeeting={() => {}}
      onRetry={() => {}}
      nowMs={nowMs}
      {...overrides}
    />,
  );

describe("Needs you", () => {
  test("puts a promise, a question and a waiting release in one list", () => {
    const markup = needsYou({
      openLoops: { status: "loaded", entries: [openLoop] },
      learning: [suggestion],
      update: updateWaiting,
    });

    expect(markup).toContain("Send Priya the revised timeline");
    expect(markup).toContain("You say open paren a lot. Write it as (?");
    expect(markup).toContain(
      "Sona 1.1.0 is available. This install is on 1.0.0.",
    );
    /* Three rows, one list, one surface: the three cards this replaced drew
     * three borders around three separate headings for one question — "what do
     * I have to deal with". */
    expect(occurrences(markup, "<li")).toBe(3);
    expect(occurrences(markup, "<ul")).toBe(1);
    expect(occurrences(markup, "rounded-card")).toBe(1);
    expect(markup.includes("What Sona noticed")).toBe(false);
    expect(markup.includes("Open loops")).toBe(false);
  });

  test("says nothing needs you as one line, not an empty surface", () => {
    const markup = needsYou();

    expect(markup).toContain("Nothing needs you.");
    expect(markup.includes("rounded-card")).toBe(false);
    expect(markup.includes("<ul")).toBe(false);
    expect(markup).toContain("text-[13px] leading-5 text-gray-800");
  });

  test("draws nothing at all until the read has answered", () => {
    expect(needsYou({ openLoops: { status: "loading" } })).toBe("");
  });

  /* The suggestion read and the update check answer on their own clock, later
   * than the loop read this section used to wait on alone. An empty page while
   * either is still out is not yet the claim this sentence makes, and the
   * sentence flashing then taking itself back is what a reader saw. */
  test("stays blank while the suggestion read is still out", () => {
    expect(needsYou({ learning: null })).toBe("");
  });

  test("stays blank while the update check is still out", () => {
    expect(needsYou({ updateChecked: false })).toBe("");
  });

  /* And a row that has arrived is drawn while the slower two are still out:
   * the wait belongs to the empty sentence, not to the list. */
  test("shows a promise that arrived before the slower reads answered", () => {
    const markup = needsYou({
      openLoops: { status: "loaded", entries: [openLoop] },
      learning: null,
      updateChecked: false,
    });

    expect(markup).toContain("Send Priya the revised timeline");
  });

  test("keeps a failed read visible instead of claiming the list is empty", () => {
    const markup = needsYou({ openLoops: { status: "error" } });

    expect(markup).toContain(LOAD_ERROR);
    expect(markup).toContain('role="alert"');
    expect(markup).toContain("Retry");
    expect(markup.includes("Nothing needs you.")).toBe(false);
  });

  test("opens the meeting a promise was made in, and names it", () => {
    const markup = needsYou({
      openLoops: { status: "loaded", entries: [openLoop] },
    });

    expect(markup).toContain('data-meeting-id="meeting-a"');
    expect(markup).toContain('aria-label="Open meeting Weekly sync"');
    expect(markup).toContain("Weekly sync");
    expect(markup).toContain("40 minutes ago");
  });

  test("leaves a promise whose meeting is gone as a line, not a dead button", () => {
    const markup = needsYou({
      openLoops: {
        status: "loaded",
        entries: [{ ...openLoop, meeting_id: "" }],
      },
    });

    expect(markup).toContain("Send Priya the revised timeline");
    expect(markup.includes("<button")).toBe(false);
  });

  test("offers both answers to a mined habit and only a dismissal to advice", () => {
    const asked = needsYou({ learning: [suggestion] });
    const observed = needsYou({ learning: [advice] });

    expect(asked).toContain("Yes");
    expect(asked).toContain("No thanks");
    /* One row: the question, and the evidence that earned it. The quoted
     * example the card printed underneath said the same thing twice. */
    expect(asked).toContain("7 times, across 3 days");
    expect(occurrences(asked, "<li")).toBe(1);

    expect(observed).toContain(
      "Dictations on Zoom get retried 2.4× more often than the rest.",
    );
    expect(observed).toContain("No thanks");
    expect(observed.includes(">Yes<")).toBe(false);
  });

  test("offers the release and a dismissal, and drops the link when there is none", () => {
    const withUrl = needsYou({ update: updateWaiting });
    const withoutUrl = needsYou({ update: { ...updateWaiting, url: null } });

    expect(withUrl).toContain("View release");
    expect(withUrl).toContain('aria-label="Dismiss"');
    expect(withoutUrl.includes("View release")).toBe(false);
    expect(withoutUrl).toContain('aria-label="Dismiss"');
  });

  test("stays quiet about an install that is already current", () => {
    const markup = needsYou({
      update: {
        ...updateWaiting,
        latest_version: null,
        update_available: false,
        url: null,
        status: "up_to_date",
      },
    });

    expect(markup).toContain("Nothing needs you.");
    expect(markup.includes("is available")).toBe(false);
  });

  test("says nothing about a check that never came back", () => {
    const markup = needsYou({
      update: {
        ...updateWaiting,
        latest_version: null,
        update_available: false,
        url: null,
        status: "check_failed",
        error: "network unreachable",
      },
    });

    expect(markup).toContain("Nothing needs you.");
    expect(markup.includes("Could not check for updates")).toBe(false);
    expect(markup.includes("network unreachable")).toBe(false);
  });
});

const recent = (
  overrides: Partial<React.ComponentProps<typeof Recent>> = {},
): string =>
  render(
    <Recent
      receipts={{ status: "loaded", entries: [] }}
      onOpenMeeting={() => {}}
      onRetry={() => {}}
      nowMs={nowMs}
      {...overrides}
    />,
  );

describe("Recent", () => {
  test("is one closed row whose fact counts today's passes", () => {
    const markup = recent({
      receipts: {
        status: "loaded",
        entries: [skippedRecording, linkedPeople, oldRun],
      },
    });

    expect(markup).toContain("Recent");
    expect(markup).toContain("2 today");
    expect(markup).toContain("<details");
    /* Closed until somebody opens it: the summary's count is what decides. */
    expect(markup.includes("<details open")).toBe(false);
    expect(markup.includes("What Sona did")).toBe(false);
  });

  test("names each pass, its workflow and when it ran", () => {
    const markup = recent({
      receipts: { status: "loaded", entries: [skippedRecording, linkedPeople] },
    });

    expect(markup).toContain("Skipped recording a detected meeting");
    expect(markup).toContain("Remembered 2 people");
    expect(markup).toContain("8 minutes ago");
    expect(markup).toContain("40 minutes ago");
    expect(markup).toContain('data-meeting-id="meeting-a"');
  });

  test("drops a pass that changed nothing, and does not count it", () => {
    const markup = recent({
      receipts: { status: "loaded", entries: [quietRun, linkedPeople] },
    });

    expect(markup).toContain("Remembered 2 people");
    expect(markup).toContain("1 today");
    expect(markup.includes("Noticed 0 things")).toBe(false);
    expect(occurrences(markup, "<li")).toBe(1);
  });

  test("leaves a pass with nothing to open as a line", () => {
    const markup = recent({
      receipts: { status: "loaded", entries: [skippedRecording] },
    });

    expect(markup).toContain("Skipped recording a detected meeting");
    expect(markup.includes("<button")).toBe(false);
  });

  test("teaches what fills it instead of showing an empty list", () => {
    const markup = recent();

    expect(markup).toContain("Nothing yet");
    expect(markup).toContain(
      "People, words and follow-ups Sona files after a meeting show up here.",
    );
    expect(markup.includes("<ul")).toBe(false);
  });

  test("claims no count while the read is out or after it failed", () => {
    expect(recent({ receipts: { status: "loading" } })).toContain("Loading…");
    expect(recent({ receipts: { status: "loading" } }).includes("today")).toBe(
      false,
    );

    const failed = recent({ receipts: { status: "error" } });
    expect(failed).toContain(LOAD_ERROR);
    expect(failed).toContain("Retry");
    expect(failed.includes("Nothing yet")).toBe(false);
  });
});
