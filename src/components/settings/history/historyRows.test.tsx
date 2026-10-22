import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  ContextReceipt,
  HistoryEntry as HistoryEntryRow,
  HistoryRunReceipt,
  HistoryStats,
  ModeReceipt,
} from "@/bindings";
import { startOfLocalDay } from "@/lib/utils/localDay";
import { HistoryEntryComponent } from "./HistoryEntry";
import {
  HistoryFeed,
  planDeepLinkStep,
  type DeepLinkProgress,
} from "./HistoryFeed";
import { HistoryRowControls } from "./HistoryRowControls";
import { historyRowActions } from "./historyRowActions";
import { HistorySummary } from "./HistorySummary";
import type { ListState } from "./historyListReducer";

/* History data, action eligibility, accessible expansion, and bounded links. */

const localeFile = path.join(
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
    en: { translation: JSON.parse(fs.readFileSync(localeFile, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);

const occurrences = (markup: string, needle: string) =>
  markup.split(needle).length - 1;

const CONTEXT: ContextReceipt = {
  requested_policy: "target",
  policy: "target",
  accessibility: "granted",
  sources: {
    target: "captured",
    focused_field: "captured",
    selected_text: "not_requested",
    browser_url: "not_requested",
    clipboard: "not_requested",
  },
  captured_at_ms: 1_756_000_000_000,
};

const MODE: ModeReceipt = {
  run_id: 7,
  settings_revision: 12,
  mode_id: "email",
  tone: "semi_formal",
  requested_context_policy: "target",
  context_policy_ceiling: "full",
  context_policy: "target",
  prompt_preset: "email",
  post_process_requested: false,
  provider_id: null,
  model_id: null,
  engine_requested: "local",
  /* A real decode always names the engine it ran on; only the failure path
   * leaves this unset. It is the discriminator the empty-text line reads. */
  engine_used: "local",
};

const ENTRY: HistoryEntryRow = {
  id: 41,
  file_name: "sona-1787979738.wav",
  timestamp: 1_756_000_000,
  saved: false,
  title: "",
  transcription_text: "Ship the dense Library rows today.",
  post_processed_text: null,
  post_process_requested: false,
  parent_id: null,
};

const receipt = (
  overrides: Partial<HistoryRunReceipt> = {},
): HistoryRunReceipt => ({
  id: 3,
  history_id: ENTRY.id,
  run_id: 7,
  retry_of_run_id: null,
  started_at_ms: 1_756_000_000_000,
  completed_at_ms: 1_756_000_001_000,
  mode: MODE,
  context: CONTEXT,
  duration_ms: 15_000,
  word_count: 6,
  source_kind: "microphone",
  has_audio: true,
  capture_status: "complete",
  delivery_attempts: [],
  ...overrides,
});

const noop = async () => undefined;
const noText = async () => undefined;
const noBlob = async () => null;

const row = (
  overrides: {
    entry?: Partial<HistoryEntryRow>;
    receipts?: HistoryRunReceipt[] | null | undefined;
    expanded?: boolean;
  } = {},
) =>
  render(
    <HistoryEntryComponent
      entry={{ ...ENTRY, ...overrides.entry }}
      receipts={"receipts" in overrides ? overrides.receipts : [receipt()]}
      view="processed"
      expanded={overrides.expanded ?? false}
      onToggleExpanded={() => undefined}
      onToggleSaved={noop}
      onCopyText={noText}
      getAudioBlob={noBlob}
      deleteAudio={noop}
      retryTranscription={noop}
    />,
  );

/** The collapsed row's one line and its two measured cells — the whole button. */
const lineOf = (markup: string): string =>
  markup.slice(
    markup.indexOf('data-testid="history-entry-toggle"'),
    markup.indexOf("</button>"),
  );

describe("library row, the calm line", () => {
  const markup = row();

  test("states the first words, the word count and the clock time", () => {
    const line = lineOf(markup);
    expect(line).toContain("Ship the dense Library rows today.");
    expect(line).toContain('data-testid="history-entry-transcript"');
    expect(line).toContain("6 words");
    // A clock time, tabular, right of everything else.
    expect(line).toMatch(/data-testid="history-entry-time"[^>]*>[^<]*\d:\d\d/);
  });

  test("carries no control at rest — the row itself is the only button", () => {
    expect(occurrences(markup, "<button")).toBe(1);
    expect(markup).toContain('data-testid="history-entry-toggle"');
    expect(markup).toContain('aria-expanded="false"');
    for (const control of [
      "copy",
      "retry",
      "delete",
      "actions",
      "details",
      "controls",
    ]) {
      expect(markup).not.toContain(`data-testid="history-entry-${control}"`);
    }
    expect(markup).not.toContain('data-testid="history-receipts"');
    expect(markup).not.toContain('data-testid="audio-player-toggle"');
  });

});

describe("library row, no-speech capture", () => {
  const silent = receipt({
    capture_status: "no_speech_detected",
    word_count: 0,
    duration_ms: 1_140,
    mode: { ...MODE, input_peak: 0.0119, input_rms: 0.0024 },
  });
  const markup = row({ receipts: [silent] });

  test("says so on the line, with the levels behind the expander", () => {
    expect(markup).toContain("No speech detected");
  });

  test("does not claim a word count for a capture with no speech", () => {
    expect(markup).not.toContain("0 words");
    expect(markup).not.toContain('data-testid="history-entry-words"');
  });

  test("keeps the player, because the retained sample is the evidence", () => {
    expect(row({ receipts: [silent], expanded: true })).toContain(
      'data-testid="audio-player-seek"',
    );
  });
});

describe("library row, opened", () => {
  const markup = row({ expanded: true });

  test("reveals playback, copy and transcribe again", () => {
    expect(markup).toContain('data-testid="history-entry-details"');
    expect(markup).toContain('data-testid="audio-player-toggle"');
    expect(markup).toContain('data-testid="history-entry-copy"');
    expect(markup).toContain('data-testid="history-entry-retry"');
    expect(markup).toContain("Copy");
    expect(markup).toContain("Transcribe again");
  });

  test("the row it opens is the region it announces", () => {
    expect(markup).toContain('aria-expanded="true"');
    const controls = /aria-controls="([^"]+)"/.exec(markup)?.[1];
    expect(controls).toBeDefined();
    expect(markup).toContain(`id="${controls}"`);
  });

  test("carries the run receipt, and the length at the player", () => {
    expect(markup).toContain('data-testid="history-receipts"');
    expect(markup).toContain('type="range"');
    expect(markup).toContain('max="15"');
    expect(markup).toContain("15s");
  });

  test("drops the player when the receipt says there is nothing to play", () => {
    const empty = row({
      expanded: true,
      receipts: [
        receipt({ capture_status: "no_speech_detected", duration_ms: 0 }),
      ],
    });
    expect(empty).not.toContain('data-testid="audio-player-seek"');
    expect(empty).not.toContain('data-testid="audio-player-toggle"');

    const noFile = row({
      expanded: true,
      receipts: [receipt({ has_audio: false })],
    });
    expect(noFile).not.toContain('data-testid="audio-player-seek"');
  });

  test("offers the player when no receipt can prove the row is empty", () => {
    expect(row({ expanded: true, receipts: [] })).toContain(
      'data-testid="audio-player-toggle"',
    );
    expect(row({ expanded: true, receipts: null })).toContain(
      'data-testid="audio-player-toggle"',
    );
  });

});

describe("library row, the action bar", () => {
  const controls = (
    overrides: { hasText?: boolean; busy?: boolean; showCopied?: boolean } = {},
  ) =>
    render(
      <HistoryRowControls
        menuActions={[]}
        hasText={overrides.hasText ?? true}
        busy={overrides.busy ?? false}
        showCopied={overrides.showCopied ?? false}
        onCopy={() => undefined}
        onRetranscribe={() => undefined}
        onDelete={() => undefined}
      />,
    );

  /* The button's own tag, not its class list. Every vg button carries
   * `disabled:pointer-events-none` in its classes, so a substring search for
   * "disabled" passes on a button that is perfectly enabled — the check has to
   * be the rendered attribute. */
  const buttonFor = (markup: string, id: string) =>
    markup.slice(
      markup.lastIndexOf("<button", markup.indexOf(`history-entry-${id}`)),
      markup.indexOf(">", markup.indexOf(`history-entry-${id}`)),
    );

  const isDisabled = (markup: string, id: string) =>
    buttonFor(markup, id).includes('disabled=""');

  test("copy is disabled when there is no text to copy", () => {
    expect(isDisabled(controls({ hasText: false }), "copy")).toBe(true);
    expect(isDisabled(controls(), "copy")).toBe(false);
  });

  test("a row mid-retry or mid-delete offers no operation at all", () => {
    const busy = controls({ busy: true });
    for (const id of ["copy", "retry"]) {
      expect(isDisabled(busy, id)).toBe(true);
    }
    expect(isDisabled(controls(), "retry")).toBe(false);
  });

  test("copy confirms itself in place instead of raising a toast", () => {
    expect(controls({ showCopied: true })).toContain("Copied");
    expect(controls()).not.toContain("Copied");
  });

  /* The bar is what the row shows at rest once it is open, and none of it can
   * destroy the entry: deleting one lives with correcting and reprocessing it,
   * behind the menu trigger. */
  test("the bar carries two named actions and no destructive one", () => {
    const markup = controls();
    expect(markup).toContain('data-testid="history-entry-copy"');
    expect(markup).toContain('data-testid="history-entry-retry"');
    expect(markup).not.toContain(">Delete<");
    expect(markup).toContain('data-testid="history-entry-actions"');
  });
});

describe("library row, the menu behind the bar", () => {
  const actions = (
    overrides: { saved?: boolean; hasText?: boolean; busy?: boolean } = {},
  ) =>
    historyRowActions({
      t: i18n.t.bind(i18n),
      saved: false,
      hasText: true,
      busy: false,
      onCorrect: () => undefined,
      onToggleSaved: () => undefined,
      onProcessAgain: () => undefined,
      ...overrides,
    });

  test("holds what changes the entry without being why you opened it", () => {
    expect(actions().map((action) => action.id)).toEqual([
      "correct",
      "save",
      "process-again",
    ]);
  });

  test("names the save action by what pressing it does", () => {
    const label = (saved: boolean) =>
      actions({ saved }).find((action) => action.id === "save")?.label;
    expect(label(false)).toBe("Save entry");
    expect(label(true)).toBe("Remove from saved");
  });

  test("disables correction when there is no text to act on, and nothing else", () => {
    expect(
      actions({ hasText: false })
        .filter((action) => action.disabled)
        .map((action) => action.id),
    ).toEqual(["correct"]);
  });

  test("a row mid-retry or mid-delete offers no operation at all", () => {
    expect(actions({ busy: true }).every((action) => action.disabled)).toBe(
      true,
    );
  });
});

describe("library row, empty transcript", () => {
  const empty = { transcription_text: "" };
  const markup = row({ entry: empty });

  test("a run the model heard and post-processing emptied is not a failure", () => {
    expect(markup).toContain("No text was recorded for this entry.");
    expect(markup).not.toContain("Transcription failed");
  });

  test("a run whose engine never reported still says the engine failed", () => {
    const failed = row({
      entry: empty,
      receipts: [receipt({ mode: { ...MODE, engine_used: null } })],
    });
    expect(failed).toContain("Transcription failed, so nothing was recorded.");
    expect(failed).not.toContain("No text was recorded for this entry.");
  });

  test("a held cloud run says what was held and why", () => {
    const held = row({
      entry: empty,
      receipts: [
        receipt({
          mode: { ...MODE, cloud_status: "held_cloud_unavailable" },
        }),
      ],
    });
    expect(held).toContain(
      "Sona held the cloud result: nothing trustworthy came back and no local model was available.",
    );
    expect(held).not.toContain("Transcription failed");
  });

  test("a truncated capture is not called a transcription failure", () => {
    // A truncated prefix is never auto-transcribed, so there was no
    // transcription to fail — and its receipt carries no engine either.
    const truncated = row({
      entry: empty,
      receipts: [
        receipt({
          capture_status: "truncated",
          mode: { ...MODE, engine_used: null },
        }),
      ],
    });
    expect(truncated).toContain("No text was recorded for this entry.");
    expect(truncated).not.toContain("Transcription failed");
  });

  test("a row from before capture_status existed is not accused of failing", () => {
    const legacy = row({
      entry: empty,
      receipts: [
        receipt({ capture_status: null, mode: { ...MODE, engine_used: null } }),
      ],
    });
    expect(legacy).toContain("No text was recorded for this entry.");
    expect(legacy).not.toContain("Transcription failed");
  });
});

describe("library row, search provenance", () => {
  test("marks only the row whose meaning matched", () => {
    const semantic = row({ entry: { match_kind: "semantic" } });
    expect(semantic).toContain("by meaning");
    expect(row({ entry: { match_kind: "text" } })).not.toContain("by meaning");
    expect(row()).not.toContain("by meaning");
  });
});

/* The day groups themselves are checked at src/lib/utils/localDay.test.ts:
 * the bucketer and the heading are shared with meeting history now, so they
 * are not the Library's to pin. What stays here is the feed's use of them. */

describe("library feed", () => {
  const NOON = 12 * 60 * 60 * 1000;
  const today = startOfLocalDay(Date.now()) + NOON;
  const yesterday = (() => {
    const date = new Date(today);
    date.setDate(date.getDate() - 1);
    return date.getTime();
  })();

  const feed = (
    state: Partial<ListState> = {},
    focusRequest: { historyId: number; nonce: number } | null = null,
  ) =>
    render(
      <HistoryFeed
        state={{
          phase: "ready",
          hasMore: false,
          entries: [
            { ...ENTRY, id: 3, timestamp: Math.floor(today / 1000) },
            { ...ENTRY, id: 2, timestamp: Math.floor(yesterday / 1000) },
          ],
          ...state,
        }}
        setQuery={() => undefined}
        view="processed"
        activeQuery=""
        focusRequest={focusRequest}
        sentinelRef={{ current: null }}
        receiptsByHistoryId={{}}
        toggleSaved={noop}
        copyToClipboard={noText}
        getAudioBlob={noBlob}
        deleteEntry={noop}
        retryHistoryEntry={noop}
        fetchPage={noop}
      />,
    );

  test("renders one named day section per day, each over its own surface", () => {
    const markup = feed();
    expect(occurrences(markup, 'data-testid="history-day"')).toBe(2);
    expect(occurrences(markup, 'data-testid="history-day-heading"')).toBe(2);
    expect(markup).toContain(">Today</h2>");
    expect(markup).toContain(">Yesterday</h2>");
    // Each day's rows sit on one hairline surface, and the surface is the list.
    expect(occurrences(markup, '<ul role="list"')).toBe(2);
    expect(occurrences(markup, 'data-testid="history-entry"')).toBe(2);
    // The heading names the list it heads, for anyone who cannot see it.
    expect(markup).toContain('aria-label="Today"');
  });

  test("marks the deep-linked dictation row for focus", () => {
    const markup = feed({}, { historyId: 3, nonce: 1 });
    expect(markup).toMatch(
      /data-history-id="3"[^>]*data-deeplink-target="true"/,
    );
    expect(occurrences(markup, 'data-deeplink-target="true"')).toBe(1);
  });

  test("the paging trip wire and its manual fallback survive the grouping", () => {
    const markup = feed({ hasMore: true });
    expect(markup).toContain('data-testid="history-load-more"');
    // The footer sits outside every day section: the next page may open a new
    // day above it.
    expect(markup.lastIndexOf("</section>")).toBeLessThan(
      markup.indexOf('data-testid="history-load-more"'),
    );
  });

  test("an empty library states what it is and how to fill it", () => {
    const markup = feed({ entries: [] });
    expect(markup).toContain("Dictations you make appear here.");
    expect(markup).not.toContain('data-testid="history-day"');
  });

  test("a day's wordless recordings collapse into one line that opens", () => {
    const silent = {
      ...ENTRY,
      transcription_text: "",
      post_processed_text: null,
    };
    const markup = feed({
      entries: [
        { ...ENTRY, id: 3, timestamp: Math.floor(today / 1000) },
        { ...silent, id: 8, timestamp: Math.floor(today / 1000) },
        { ...silent, id: 9, timestamp: Math.floor(today / 1000) },
      ],
    });
    expect(occurrences(markup, 'data-testid="history-empty-day"')).toBe(1);
    expect(markup).toContain("2 empty recordings");
    // Closed, and closed by default: the reader asked for the day, not for
    // three rows two of which say nothing.
    expect(markup).not.toMatch(/<details[^>]*\sopen/);
    // The rows still exist, inside the line, and the one with words does not.
    expect(occurrences(markup, 'data-testid="history-entry"')).toBe(3);
    expect(markup.indexOf('data-history-id="3"')).toBeLessThan(
      markup.indexOf('data-testid="history-empty-day"'),
    );
  });

  test("the row a dictation link names is never collapsed out of reach", () => {
    const silent = {
      ...ENTRY,
      transcription_text: "",
      post_processed_text: null,
    };
    const markup = feed(
      {
        entries: [
          { ...silent, id: 8, timestamp: Math.floor(today / 1000) },
          { ...silent, id: 9, timestamp: Math.floor(today / 1000) },
        ],
      },
      { historyId: 8, nonce: 1 },
    );
    // A row inside a closed `<details>` cannot be scrolled to or focused, so
    // the linked row stays a row and only its neighbour collapses.
    expect(markup).toContain("1 empty recording");
    expect(markup.indexOf('data-deeplink-target="true"')).toBeLessThan(
      markup.indexOf('data-testid="history-empty-day"'),
    );
  });
});

/* The dictation deep link, asserted as the decision it is rather than as a
 * sequence of renders.
 *
 * `App.tsx` sets the request in its `queryLinkRequested` listener and never
 * clears it, and reading it more than once shipped three defects: the History
 * search box cleared itself 200 ms after the user typed into it, the expand
 * replayed on every return to the section, and a link naming a row retention
 * had swept walked every page of the log. Static rendering runs no effects,
 * so what is pinned here is the planner the effect applies. */
describe("library feed, dictation deep link", () => {
  const LINK = { historyId: 42, nonce: 3 };

  const plan = (
    overrides: Partial<Parameters<typeof planDeepLinkStep>[0]> = {},
  ) =>
    planDeepLinkStep({
      focusRequest: LINK,
      progress: null,
      activeQuery: "",
      entries: [{ id: 42 }, { id: 41 }],
      phase: "ready",
      hasMore: false,
      ...overrides,
    });

  test("a link that has been served never clears the search again", () => {
    const served = plan({
      progress: { nonce: 3, satisfied: true, pagesLoaded: 0 },
      activeQuery: "budget",
    });
    expect(served.step).toEqual({ kind: "none" });
    // A keystroke in the search box is what re-ran this decision, so the
    // query the user is looking at has to survive it.
    expect(served.progress).toEqual({
      nonce: 3,
      satisfied: true,
      pagesLoaded: 0,
    });
  });

  test("a link not yet served clears the search, then expands its row", () => {
    const searching = plan({ activeQuery: "budget" });
    expect(searching.step).toEqual({ kind: "clear-search" });
    expect(searching.progress?.satisfied).toBe(false);

    const reloaded = plan({ progress: searching.progress });
    expect(reloaded.step).toEqual({ kind: "expand", historyId: 42 });
    expect(reloaded.progress?.satisfied).toBe(true);
  });

  test("a newer link starts over where the previous one stopped", () => {
    const relinked = plan({
      focusRequest: { historyId: 42, nonce: 4 },
      progress: { nonce: 3, satisfied: true, pagesLoaded: 6 },
      entries: [{ id: 9 }, { id: 8 }],
      hasMore: true,
    });
    expect(relinked.step).toEqual({ kind: "load-page", cursor: 8 });
    expect(relinked.progress).toEqual({
      nonce: 4,
      satisfied: false,
      pagesLoaded: 1,
    });
  });

  test("the walk stops after six pages instead of mounting the log", () => {
    // Driven the way the effect drives it: its own progress back in, one more
    // row loaded per page, and the named row nowhere in the log.
    let progress: DeepLinkProgress | null = null;
    let oldest = 100;
    let entries = [{ id: oldest }];
    const cursors: number[] = [];

    for (let tick = 0; tick < 40; tick += 1) {
      const walked = planDeepLinkStep({
        focusRequest: LINK,
        progress,
        activeQuery: "",
        entries,
        phase: "ready",
        hasMore: true,
      });
      progress = walked.progress;
      if (walked.step.kind !== "load-page") break;
      cursors.push(walked.step.cursor);
      oldest -= 1;
      entries = [...entries, { id: oldest }];
    }

    // Six pages, each cursored on the oldest row loaded so far.
    expect(cursors).toEqual([100, 99, 98, 97, 96, 95]);
    // Spent, so the next history event does not start the walk over.
    expect(progress?.satisfied).toBe(true);
  });

  test("a row the log no longer holds is given up on, not waited for", () => {
    const missing = plan({ entries: [{ id: 9 }], hasMore: false });
    expect(missing.step).toEqual({ kind: "none" });
    expect(missing.progress?.satisfied).toBe(true);
  });

  test("a page in flight is waited out with the link still armed", () => {
    const paging = plan({
      entries: [{ id: 9 }],
      hasMore: true,
      phase: "paging",
    });
    expect(paging.step).toEqual({ kind: "none" });
    expect(paging.progress?.satisfied).toBe(false);
  });
});

describe("library summary line", () => {
  const stats: HistoryStats = {
    entries: 4,
    total_duration_ms: 15_000,
    total_words: 96,
    by_source: [],
  };

  const summary = (overrides: Partial<HistoryStats> = {}) =>
    render(
      <HistorySummary
        stats={{ ...stats, ...overrides }}
        loading={false}
        error={false}
        onRetry={() => undefined}
      />,
    );

  test("states the three totals as one quiet sentence", () => {
    const markup = summary();
    expect(markup).toContain("4 recordings · 15s · 96 words");
    expect(markup).toContain('data-testid="history-summary"');
  });

  test("reports a 15-second library as 15s, never as 0h 0m", () => {
    expect(summary()).not.toContain("0h 0m");
    expect(summary({ total_duration_ms: 3_840_000 })).toContain("1h 4m");
  });


  test("counts one recording without pretending it is several", () => {
    expect(summary({ entries: 1, total_words: 1 })).toContain(
      "1 recording · 15s · 1 word",
    );
  });

  test("while loading it invents no figure at all", () => {
    const markup = render(
      <HistorySummary
        stats={null}
        loading
        error={false}
        onRetry={() => undefined}
      />,
    );
    expect(markup).toContain('data-testid="history-summary-loading"');
    /* No rendered text at all, so no figure the backend never reported and no
     * half-sentence with blanks in it. The only visible thing is the bar; the
     * one string in the markup is the region's accessible name. */
    expect(markup).not.toMatch(/>[^<]*\d/);
    expect(markup).toContain('aria-label="Counting your recordings…"');
  });

});

