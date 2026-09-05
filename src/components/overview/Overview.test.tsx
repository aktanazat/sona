import {
  afterAll,
  afterEach,
  beforeEach,
  describe,
  expect,
  test,
} from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  DictationRecordingChangedEvent,
  HistoryTrendProjection,
} from "@/bindings";
import { TooltipProvider } from "@/components/vg/tooltip";
import { ActivityBand } from "./ActivityBand";
import { activityPage } from "./activityPaging";
import {
  CaptureHero,
  Overview,
  subscribeToRecordingState,
  type CaptureHeroProps,
} from "./Overview";

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
 * inspected. The IPC internals beside it belong to the recording-state block,
 * the one place in this file that reaches the backend seam — no render does,
 * because `renderToStaticMarkup` runs no effect.
 *
 * Installed at module scope (a module-scope render below needs it at import
 * time) and RESTORED in afterAll: a leaked bare `window` makes every later
 * test file in the same process believe it is in a browser — framer-motion
 * then initialises its reduced-motion listener against a window with no
 * `matchMedia` and pins the device preference to false, which broke
 * motion.test.tsx only in full-suite order. */
const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");

/* The one event this page follows, in the envelope the event plugin hands a
 * listener: the generated payload type under the plugin's own header. */
type RecordingEnvelope = {
  event: string;
  id: number;
  payload: DictationRecordingChangedEvent;
};
type RecordingHandler = (envelope: RecordingEnvelope) => void;
/* Everything the page sends the host: the listen registration's handler id
 * and event name, or nothing for `is_recording`. */
type HostArgs = { handler?: number; event?: string };
type HostReply = boolean | number | null;

/* The backend seam, driven the way the Tauri runtime drives it: `listen()`
 * leaves as a `plugin:event|listen` invocation carrying a callback id, and a
 * transition arrives later as a call to that callback. Nothing stands in for
 * `@/bindings`, so the event name and payload field asserted below are the
 * generated ones — rename the Rust event without re-exporting bindings and
 * these tests stop seeing transitions. */
const backend = {
  /** Every command the page sent, in order. A reinstated poll shows up here. */
  invoked: new Array<string>(),
  /** What `is_recording` answers, and when it answers it. */
  recording: Promise.resolve(false),
  /** Callback id to the handler the event plugin will call. */
  callbacks: new Map<number, RecordingHandler>(),
  /** Event name to the handler registered for it. */
  handlers: new Map<string, RecordingHandler>(),
  /** Repeating timers scheduled through the window. */
  intervals: 0,
};

Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: {
    __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" },
    __TAURI_INTERNALS__: {
      transformCallback: (callback: RecordingHandler): number => {
        const id = backend.callbacks.size + 1;
        backend.callbacks.set(id, callback);
        return id;
      },
      invoke: (command: string, args: HostArgs = {}): Promise<HostReply> => {
        backend.invoked.push(command);
        switch (command) {
          case "plugin:event|listen": {
            const handler = backend.callbacks.get(Number(args.handler));
            if (!handler)
              throw new Error(`no callback ${String(args.handler)}`);
            backend.handlers.set(String(args.event), handler);
            return Promise.resolve(backend.callbacks.size);
          }
          case "plugin:event|unlisten":
            backend.handlers.delete(String(args.event));
            return Promise.resolve(null);
          case "is_recording":
            return backend.recording;
          default:
            throw new Error(`unstubbed command: ${command}`);
        }
      },
    },
    __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: () => {} },
    setInterval: (): number => {
      backend.intervals += 1;
      return 0;
    },
    clearInterval: () => {},
  },
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

const RECORDING_EVENT = "dictation-recording-changed-event";

/* Every mocked invocation above resolves immediately, so one macrotask hop
 * drains every microtask the subscription queues. Nothing here waits on a
 * clock, because nothing in the subscription reads one. */
const settled = (): Promise<void> =>
  /* The executor form, because `Promise.withResolvers` is ES2024 and this
   * project compiles against ES2020 (tsconfig.json). */
  new Promise<void>((resolve) => {
    setImmediate(() => resolve());
  });

const emitRecording = (recording: boolean): void => {
  const handler = backend.handlers.get(RECORDING_EVENT);
  if (!handler) throw new Error("nothing is following recording transitions");
  handler({ event: RECORDING_EVENT, id: 1, payload: { recording } });
};

/* The state word is the whole reason this page talked to the backend on a
 * timer for a year. It now follows one broadcast, so what has to hold is that
 * every transition arrives, that nothing is scheduled to ask again, and that
 * the one mount read cannot overwrite a newer answer. */
describe("the Capture page's recording state", () => {
  const realSetInterval = globalThis.setInterval;

  beforeEach(() => {
    backend.invoked.length = 0;
    backend.callbacks.clear();
    backend.handlers.clear();
    backend.intervals = 0;
    backend.recording = Promise.resolve(false);
    /* The poll this replaced went through `window.setInterval`; a bare
     * `setInterval` would miss the window stub, so both are counted. Neither
     * schedules anything: a reinstated poll fails on the count, and a live
     * timer would only leak into the next test. The widened alias exists
     * because a counting stub cannot satisfy the host's Timer-returning
     * signature. */
    const mutableGlobals: { setInterval: unknown } = globalThis;
    mutableGlobals.setInterval = (): number => {
      backend.intervals += 1;
      return 0;
    };
  });

  afterEach(() => {
    globalThis.setInterval = realSetInterval;
  });

  test("follows every transition the backend announces", async () => {
    const seen: boolean[] = [];
    const stop = subscribeToRecordingState((recording) => seen.push(recording));
    await settled();

    emitRecording(true);
    emitRecording(false);

    expect(seen).toEqual([false, true, false]);
    stop();
  });

  test("reads the state once and schedules nothing to ask again", async () => {
    const stop = subscribeToRecordingState(() => {});
    await settled();
    emitRecording(true);
    await settled();

    expect(
      backend.invoked.filter((command) => command === "is_recording"),
    ).toEqual(["is_recording"]);
    expect(backend.intervals).toBe(0);
    stop();
  });

  test("keeps a transition that arrives while the mount read is out", async () => {
    let answerRead!: (recording: boolean) => void;
    backend.recording = new Promise<boolean>((resolve) => {
      answerRead = resolve;
    });
    const seen: boolean[] = [];
    const stop = subscribeToRecordingState((recording) => seen.push(recording));
    await settled();

    emitRecording(true);
    answerRead(false);
    await settled();

    /* The stale answer must not land on top of the newer transition: no
     * interval remains to correct it on the next tick, so the page would claim
     * Ready for the rest of the dictation. */
    expect(seen).toEqual([true]);
    stop();
  });

  test("unregisters its listener when the page goes away", async () => {
    const stop = subscribeToRecordingState(() => {});
    await settled();
    expect(backend.handlers.has(RECORDING_EVENT)).toBe(true);

    stop();
    await settled();

    expect(backend.handlers.has(RECORDING_EVENT)).toBe(false);
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
