import { afterAll, describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { HistoryTrendProjection } from "@/bindings";
import { TooltipProvider } from "@/components/vg/tooltip";
import { ActivityBand } from "./ActivityBand";
import { activityPage } from "./activityPaging";
import { CaptureHero, Overview, type CaptureHeroProps } from "./Overview";

/* Capture's contract covers the hero's state, chord and actions. The activity
 * band has its own data-driven assertions below. Keys resolve through the real
 * English bundle, so a pruned key shows up as a missing sentence rather than
 * as a silent inline default.
 *
 * The chord states are rendered through `CaptureHero` rather than through the
 * page, because the page reads its settings from a zustand store and zustand
 * answers a server render with the store's *initial* state. So `Overview` can
 * only ever be statically rendered as its first paint — which is exactly what
 * the page-level block below asserts, and no place to put a bound chord. */

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

/* @tauri-apps/plugin-os reads its platform off a window global the Tauri
 * runtime injects, and the keycaps are macOS glyphs because of it. Static
 * rendering has no window, so without this the hero throws before it can be
 * inspected. Nothing else is needed: `renderToStaticMarkup` runs no effect, so
 * the page reaches no command.
 *
 * Installed at module scope (a module-scope render below needs it at import
 * time) and RESTORED in afterAll: a leaked bare `window` makes every later
 * test file in the same process believe it is in a browser — framer-motion
 * then initialises its reduced-motion listener against a window with no
 * `matchMedia` and pins the device preference to false, which broke
 * motion.test.tsx only in full-suite order. */
const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" } },
});
afterAll(() => {
  if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeRoot, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

/* The tooltip provider is the one the route root mounts, so a render without
 * it is a render the app never performs — Radix's tooltip refuses to mount
 * outside a provider. */
const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const occurrences = (markup: string, needle: string): number =>
  markup.split(needle).length - 1;

const hero = (overrides: Partial<CaptureHeroProps> = {}): string =>
  render(
    <CaptureHero
      isRecording={false}
      binding="option_left+space"
      pushToTalk={true}
      importing={false}
      onNewMeeting={() => {}}
      onImportAudio={() => {}}
      onRecordScreen={() => {}}
      onChangeShortcut={() => {}}
      onOpenModes={() => {}}
      {...overrides}
    />,
  );

describe("the Capture hero", () => {
  test("names the state the app is in, and nothing repeats it", () => {
    const markup = hero();

    expect(markup).toContain('id="overview-status"');
    expect(markup).toContain('aria-labelledby="overview-status"');
    expect(markup).toContain("Ready");
    expect(markup).toContain('aria-live="polite"');
    /* Not recording: the heading carries no marker at all — the attribute is
     * absent rather than false. The aurora does mark the state, because its
     * idle wash is a different animation from its recording breath
     * (styles/aurora.css), and that marker is the only other copy on the
     * card. */
    expect(markup.match(/data-recording/g)?.length).toBe(1);
    expect(markup).toContain('data-recording="false"');
  });

  test("switches the state word while the backend is recording", () => {
    const markup = hero({ isRecording: true });

    expect(markup).toContain("Listening");
    expect(markup).toContain('data-recording="true"');
    expect(markup.includes(">Ready<")).toBe(false);
  });

  test("draws the bound chord exactly once, with its gesture", () => {
    const markup = hero();

    /* The macOS glyph is in the keycaps and nowhere else; the spelled-out form
     * is the button's title, one hover away. */
    expect(occurrences(markup, "\u2325")).toBe(1);
    expect(occurrences(markup, "<kbd")).toBe(2);
    expect(markup).toContain(">Space</kbd>");
    expect(markup).toContain('title="Left Option + Space"');
    expect(occurrences(markup, "tap to toggle \u00b7 hold to talk")).toBe(1);
    expect(occurrences(markup, 'data-testid="overview-shortcut"')).toBe(1);
    expect(markup).toContain('aria-label="Change dictation shortcut"');
  });

  test("stops claiming a hold when push-to-talk is off", () => {
    const markup = hero({ pushToTalk: false });

    expect(occurrences(markup, "tap to toggle")).toBe(1);
    expect(markup.includes("hold to talk")).toBe(false);
  });

  test("offers to bind a chord instead of drawing empty keycaps", () => {
    const markup = hero({ binding: null });

    expect(markup).toContain("Set a shortcut");
    expect(occurrences(markup, 'data-testid="overview-shortcut"')).toBe(1);
    expect(markup.includes("<kbd")).toBe(false);
    expect(markup.includes("tap to toggle")).toBe(false);
  });
});

/* The first paint contains the hero while the history trend loads. The
 * data-driven band is rendered separately below with the command's real shape. */
describe("the Capture page", () => {
  const markup = render(<Overview onOpenRecorder={() => {}} />);

  test("reads state first and keeps the closed rows under it", () => {
    expect(markup).toContain('id="overview-status"');
    expect(markup).toContain("Recent");
    expect(markup).toContain("<details");
    expect(markup.indexOf('id="overview-status"')).toBeLessThan(
      markup.indexOf("Recent"),
    );
  });

  test("claims nothing before any read has answered", () => {
    /* Needs you draws nothing at all while its read is out, This week has no
     * trend to draw, and neither the update sentence nor a stat exists yet. */
    expect(markup.includes("Needs you")).toBe(false);
    expect(markup.includes("Nothing needs you.")).toBe(false);
    expect(markup.includes("This week")).toBe(false);
    expect(markup.includes("Dictations per day")).toBe(false);
    expect(markup.includes("is available. This install is on")).toBe(false);
  });
});

const activityTrend: HistoryTrendProjection = {
  range: "days_180",
  range_start_local_date: "2026-03-04",
  range_end_local_date: "2026-08-30",
  all_time: {
    recordings: 18,
    duration_ms: 12_000,
    words: 180,
    by_source: [],
  },
  range_total: {
    recordings: 18,
    duration_ms: 12_000,
    words: 180,
    by_source: [],
  },
  active_days: 6,
  current_streak_days: 3,
  points: [
    {
      local_date: "2026-08-23",
      recordings: 0,
      duration_ms: 0,
      words: 0,
      by_source: [],
    },
    {
      local_date: "2026-08-24",
      recordings: 1,
      duration_ms: 1000,
      words: 10,
      by_source: [],
    },
    {
      local_date: "2026-08-25",
      recordings: 4,
      duration_ms: 2000,
      words: 40,
      by_source: [],
    },
    {
      local_date: "2026-08-26",
      recordings: 2,
      duration_ms: 1000,
      words: 20,
      by_source: [],
    },
    {
      local_date: "2026-08-27",
      recordings: 0,
      duration_ms: 0,
      words: 0,
      by_source: [],
    },
    {
      local_date: "2026-08-28",
      recordings: 6,
      duration_ms: 4000,
      words: 60,
      by_source: [],
    },
    {
      local_date: "2026-08-29",
      recordings: 3,
      duration_ms: 2000,
      words: 30,
      by_source: [],
    },
    {
      local_date: "2026-08-30",
      recordings: 2,
      duration_ms: 2000,
      words: 20,
      by_source: [],
    },
  ],
};

describe("This week", () => {
  const markup = render(<ActivityBand trend={activityTrend} />);

  test("is one closed row whose summary carries the week's numbers", () => {
    expect(markup).toContain("This week");
    expect(markup).toContain("18 dictations · 180 words · 3-day streak");
    expect(markup).toContain("<details");
    /* Closed until somebody wants the shape of it: the summary is the reading,
     * the charts are the second look. */
    expect(markup.includes("<details open")).toBe(false);
  });

  test("pages backward through the retained trend in seven-day ranges", () => {
    const current = activityPage(activityTrend.points, 0);
    const previous = activityPage(activityTrend.points, 1);

    expect(current.points.map((point) => point.local_date)).toEqual([
      "2026-08-24",
      "2026-08-25",
      "2026-08-26",
      "2026-08-27",
      "2026-08-28",
      "2026-08-29",
      "2026-08-30",
    ]);
    expect(previous.points.map((point) => point.local_date)).toEqual([
      "2026-08-23",
    ]);
    expect(current.page).toBe(0);
    expect(previous.page).toBe(1);
  });

  test("sums one week into the summary, not the whole retained trend", () => {
    /* The retained trend is months wide; the fact is one week of it. Both the
     * existing fixture and this one end on 2026-08-30, but this one puts real
     * numbers in a day the week excludes, so a fact that added up every point
     * it was handed would read 23 dictations · 230 words.
     *
     * Static rendering always lands on page 0, where the paged week and this
     * week are the same slice, so this is as far as the unit surface can see
     * the distinction the label makes. */
    const markup = render(
      <ActivityBand
        trend={{
          ...activityTrend,
          points: [
            {
              local_date: "2026-08-22",
              recordings: 5,
              duration_ms: 3000,
              words: 50,
              by_source: [],
            },
            ...activityTrend.points,
          ],
        }}
      />,
    );

    expect(markup).toContain("18 dictations · 180 words · 3-day streak");
  });

  test("translates complete aria sentences for each chart", () => {
    expect(markup).toContain(
      'aria-label="Dictations per day, highest 6 on Friday"',
    );
    expect(markup).toContain(
      'aria-label="Words per day, 180 total, ending at 20"',
    );
    expect(markup).toContain(
      'aria-label="Current streak, 3 days. Active days this week:',
    );
    expect(markup).toContain('aria-label="Previous 7 days"');
    expect(markup).toContain('aria-label="Next 7 days"');
  });
});
