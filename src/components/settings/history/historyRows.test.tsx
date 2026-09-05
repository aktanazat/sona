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
import { HistorySettings } from "./HistorySettings";
import { HistorySummary } from "./HistorySummary";
import type { ListState } from "./historyListReducer";
import { PAGE_COLUMN } from "../rows";

/* What the Library is allowed to say, and what it must never say.
 *
 * The log is quiet: rows are grouped by day, a collapsed row is one line — the
 * transcript's first words, a word count, a clock time — and it carries no
 * control at all. Opening a row is what produces playback, copy, transcribe
 * again and delete.
 *
 * Defects are pinned dead here, each of which shipped and each of which a
 * plausible refactor would bring back:
 *   - a metadata line of seven fragments: date, duration, words, mode chip and
 *     provenance all on a collapsed row, with peak/rms one expander away;
 *   - the date printed on every row when the day heading above the group
 *     already states it;
 *   - three icon buttons on every row of a thirty-row list, and a player
 *     mounted on all thirty;
 *   - a player on a capture that holds nothing to play;
 *   - "0h 0m" for a library that holds real recordings, three 24px stat cards
 *     given the loudest type on a page whose subject is the list below them,
 *     a fourth/fifth card for a provenance split, and an echo sublabel;
 *   - a printed 0.0000 for an input level nobody measured;
 *   - a transcript line on a row whose receipt says there was no speech;
 *   - a `role="tablist"` on the two-segment view switch, with no tabpanel
 *     anywhere under it;
 *   - the toolbar controls (search, view switch, folder button) overlapping —
 *     the DOM order pinned here is what the honest flex wrap relies on;
 *   - a page-split day appearing twice, one group per page.
 *
 * Static rendering runs no effects, so these are pure prop-to-markup checks and
 * no Tauri command is reachable from here. The open row is reachable because
 * the list owns which row is open, so `expanded` is a prop: a real public
 * interface, rendered the way the feed renders it. Radix keeps menu and dialog
 * content in a portal that a static render never mounts, so the menu's
 * operations are asserted on `historyRowActions` — the list the menu is built
 * from — plus the trigger that opens it. */

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
    expect(line).toContain("tabular-nums");
  });

  test("does not print the date its day heading owns", () => {
    // The feed groups by day and names the day once. A row repeating it is the
    // same fact twice, and it was what made the old metadata line long.
    expect(markup).not.toContain("2025");
    expect(markup).not.toContain("Aug");
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

  test("the mode, the engine, the source and the levels are not on the row", () => {
    expect(markup).not.toContain('data-testid="history-entry-mode"');
    expect(markup).not.toContain("email");
    expect(markup).not.toContain("Local");
    expect(markup).not.toContain("Microphone");
    expect(markup).not.toContain(">peak<");
    expect(markup).not.toContain("0.0000");
  });

  test("keeps a measured input level off the collapsed row too", () => {
    const measured = row({
      receipts: [
        receipt({ mode: { ...MODE, input_peak: 0.1456, input_rms: 0.011 } }),
      ],
    });
    expect(measured).not.toContain("0.1456");
    expect(measured).not.toContain(">peak<");
  });

  test("states no duration: the opened row's player and receipt do that", () => {
    expect(markup).not.toContain("15s");
    expect(row({ receipts: [receipt({ has_audio: false })] })).not.toContain(
      "15s",
    );
  });

  test("reprocess parentage is provenance, not a row cell", () => {
    expect(row({ entry: { parent_id: 17 } })).not.toContain("from #17");
  });

  test("a count and a clock time never animate", () => {
    /* Rule 1 of the motion contract: a tweened measurement displays values
     * nothing reported. Both cells carry the blanket freeze. */
    expect(occurrences(lineOf(markup), "snap-measured")).toBe(2);
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
    expect(markup).not.toContain("0.0119");
    expect(markup).not.toContain("0.0024");
  });

  test("does not claim a word count for a capture with no speech", () => {
    expect(markup).not.toContain("0 words");
    expect(markup).not.toContain('data-testid="history-entry-words"');
  });

  test("the statement reads as the app talking, not as a transcript", () => {
    // A real transcript is the row's content and reads at full contrast;
    // everything the app says *about* the row steps back one tier.
    expect(lineOf(markup)).toContain('data-tone="stated"');
    expect(lineOf(row())).toContain('data-tone="text"');
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

  /* Reading-first doctrine: a log is a reading surface, so nothing standing on
   * it may throw a recording away in one press. Delete is a destructive item
   * in the row's menu now, and Radix keeps menu content in a portal a static
   * render never mounts — so what is pinned here is that the opened row shows
   * no destructive control at all. */
  test("keeps no destructive control on the opened row", () => {
    const bar = markup.slice(
      markup.indexOf('data-testid="history-entry-controls"'),
      markup.indexOf('data-testid="history-receipts"'),
    );
    expect(bar).not.toContain("text-red-900");
    expect(bar).not.toContain(">Delete<");
  });

  test("the two named actions carry a hairline, and only the menu is a ghost", () => {
    const bar = markup.slice(
      markup.indexOf('data-testid="history-entry-controls"'),
      markup.indexOf('data-testid="history-receipts"'),
    );
    expect(occurrences(bar, 'data-variant="outline"')).toBe(2);
    expect(occurrences(bar, 'data-variant="ghost"')).toBe(1);
    // The one ghost is the icon-only menu trigger.
    expect(bar).toContain('data-testid="history-entry-actions"');
  });

  test("the row it opens is the region it announces", () => {
    expect(markup).toContain('aria-expanded="true"');
    const controls = /aria-controls="([^"]+)"/.exec(markup)?.[1];
    expect(controls).toBeDefined();
    expect(markup).toContain(`id="${controls}"`);
  });

  test("shows the whole transcript rather than a second copy of it", () => {
    // One text node: truncated closed, wrapped open. A separate full-text
    // block under the header would print the opening words twice.
    expect(occurrences(markup, "Ship the dense Library rows today.")).toBe(1);
    expect(markup).toContain('data-expanded="true"');
  });

  test("carries the run receipt, and the length at the player", () => {
    expect(markup).toContain('data-testid="history-receipts"');
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

  test("the player row rides quiet: gray control, one tabular duration", () => {
    // The player is a frozen primitive, so the row quiets it from outside.
    // Static markup escapes the `&` in an arbitrary variant, hence the
    // suffix match rather than the literal class.
    expect(markup).toContain("_button]:text-gray-900");
    expect(markup).toContain("_span]:tabular-nums");
  });

  test("keeps a real range control rather than an unstyled nub", () => {
    expect(markup).toContain('type="range"');
    expect(markup).not.toContain("appearance-none");
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
    expect(markup).not.toContain("text-red-900");
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
    // A derived classification stays in the metadata tier rather than reading
    // as part of the transcript.
    expect(semantic).not.toContain('text-gray-1000">by meaning');

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

  test("no row is open until one is asked for", () => {
    const markup = feed();
    expect(occurrences(markup, 'aria-expanded="false"')).toBe(2);
    expect(markup).not.toContain('data-testid="history-entry-details"');
    expect(markup).not.toContain('data-testid="audio-player-toggle"');
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
    expect(markup).toContain("No recordings yet.");
    expect(markup).toContain('data-testid="history-empty-import"');
    expect(markup).not.toContain('data-testid="history-day"');
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

  test("is a line, not a band of stat cards", () => {
    const markup = summary();
    // Three 24px figures were the loudest type on a page whose subject is the
    // list below them.
    expect(markup).not.toContain('data-testid="history-stat"');
    expect(markup).not.toContain("grid-cols-3");
    expect(markup).not.toContain("text-2xl");
    expect(markup).not.toContain("<dl");
    expect(occurrences(markup, "<p")).toBe(1);
  });

  test("counts read as measurements: tabular, and never tweened", () => {
    const markup = summary();
    expect(markup).toContain("tabular-nums");
    expect(markup).toContain("snap-measured");
  });

  test("a mixed-provenance library still states one line, not five figures", () => {
    // Provenance is a property of a recording: the row that owns it states it
    // on its receipt.
    const markup = summary({
      by_source: [
        {
          source_kind: "microphone",
          entries: 3,
          total_duration_ms: 12_000,
          total_words: 80,
        },
        {
          source_kind: "file",
          entries: 1,
          total_duration_ms: 3_000,
          total_words: 16,
        },
      ],
    });
    expect(markup).toContain("4 recordings · 15s · 96 words");
    expect(markup).not.toContain("Microphone");
    expect(markup).not.toContain("File");
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

  test("the stats failure line carries a real button, not a ghost label", () => {
    const failed = render(
      <HistorySummary
        stats={null}
        loading={false}
        error
        onRetry={() => undefined}
      />,
    );
    expect(failed).toContain('data-variant="outline"');
    expect(failed).not.toContain('data-variant="ghost"');
  });
});

describe("library page chrome", () => {
  /* Static render, phase "loading": no effect runs, no Tauri command is
   * reached, and the header plus toolbar render around the skeletons. */
  const markup = render(<HistorySettings />);

  test("the page is one centred column, the shared one", () => {
    /* The measure itself belongs to `PAGE_COLUMN`, so this asserts the
     * constant rather than restating its utilities: a spacing pass that
     * changes the column changes both sides at once, while losing the column
     * or drawing a second one still fails here. */
    expect(occurrences(markup, PAGE_COLUMN)).toBe(1);
    expect(markup).toContain("pt-8");
    expect(markup).toContain("pb-[72px]");
  });

  test("the title row carries exactly one action, and not the folder button", () => {
    const header = markup.slice(
      markup.indexOf("<h1"),
      markup.indexOf('data-testid="history-toolbar"'),
    );
    expect(header).toContain('data-testid="history-import"');
    expect(header).not.toContain('data-testid="history-open-folder"');
    // One destination, one name: the h1 answers to the rail's word, and the
    // import action reuses the hero's label instead of coining a second one.
    expect(header).toContain(">Library</h1>");
    expect(header).toContain("Import audio");
    expect(header).not.toContain("History");
  });

  test("the totals sit against the title they describe", () => {
    // The summary is one line about what the page holds, so it reads under the
    // title rather than a page gap below it.
    const header = markup.slice(
      markup.indexOf("<h1"),
      markup.indexOf('data-testid="history-toolbar"'),
    );
    expect(header).toContain('data-testid="history-summary-loading"');
  });

  /* The page title's own size belongs to `PageTitle` in settings/rows.tsx and
   * is asserted where that primitive lives; pinning its class string from the
   * Library's test made a type-scale change fail in the wrong file. */

  test("no text action is left invisible at rest", () => {
    /* Secondary actions now carry a shared hairline. These standing text
     * actions remain `outline` so their hierarchy is explicit. */
    expect(markup).not.toContain('data-slot="button" data-variant="secondary"');
    const toolbar = markup.slice(
      markup.indexOf('data-testid="history-toolbar"'),
      markup.indexOf('data-testid="history-loading"'),
    );
    const folder = toolbar.slice(
      toolbar.lastIndexOf("<button", toolbar.indexOf("history-open-folder")),
      toolbar.indexOf("history-open-folder"),
    );
    expect(folder).toContain('data-variant="outline"');
    // The two ghosts that survive are icon-only: the in-field clear (absent
    // with an empty query) and the row controls, which live in the row, not here.
    expect(toolbar).not.toContain('data-variant="ghost"');
  });

  test("one toolbar owns search, the view switch and the folder button, in wrap order", () => {
    const toolbar = markup.slice(
      markup.indexOf('data-testid="history-toolbar"'),
      markup.indexOf('data-testid="history-loading"'),
    );
    const search = toolbar.indexOf('data-testid="history-search"');
    const view = toolbar.indexOf('data-testid="history-text-view"');
    const folder = toolbar.indexOf('data-testid="history-open-folder"');
    expect(search).toBeGreaterThan(-1);
    expect(view).toBeGreaterThan(-1);
    expect(folder).toBeGreaterThan(-1);
    // The honest wrap depends on this order: the growing search field first,
    // then the flex-none controls, folder last so it wraps first.
    expect(search).toBeLessThan(view);
    expect(view).toBeLessThan(folder);
  });

  test("the view switch is a two-segment control, Processed first, not a tablist", () => {
    const control = markup.slice(
      markup.indexOf('data-testid="history-text-view"'),
      markup.indexOf('data-testid="history-open-folder"'),
    );
    expect(occurrences(control, 'data-slot="toggle-group-item"')).toBe(2);
    expect(control.indexOf("Processed")).toBeLessThan(control.indexOf("Raw"));
    // Two segments over one list is not a tab structure, and claiming one
    // promises assistive tech a tabpanel that does not exist.
    expect(markup).not.toContain('role="tablist"');
    expect(markup).not.toContain('role="tab"');
    // One control, not two buttons side by side.
    expect(control).toContain('data-spacing="0"');
  });

  test("the loading list is a run of calm rows, not stacked two-line blocks", () => {
    const skeletons = markup.slice(
      markup.indexOf('data-testid="history-loading"'),
      markup.length,
    );
    // Five rows, two bars each: a transcript line and its measurement.
    expect(occurrences(skeletons, 'data-slot="skeleton"')).toBe(10);
  });
});
