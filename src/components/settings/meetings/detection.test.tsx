import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import {
  MeetingDetectionState,
  MeetingDetectionToggle,
} from "./MeetingDetectionSettings";
import { MeetingAppsPicker } from "./MeetingAppsPicker";
import { promptTitle } from "./DetectionListeners";
import {
  useDetectionStore,
  type DetectionPromptEvent,
  type DetectionPromptKind,
  type DetectionSettings,
  type DetectionStatus,
} from "./detectionStore";


const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

const catalogue = JSON.parse(fs.readFileSync(localeRoot, "utf8"));

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: catalogue } },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});

/* Every window root mounts one `TooltipProvider` and the row primitives assume
 * it, so a hinted row rendered in isolation needs its own — Radix's `Tooltip`
 * throws without one. */
const paint = (node: React.ReactElement) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

describe("first paint", () => {
  test("the master switch claims the backend default before state arrives", () => {
    const markup = paint(<MeetingDetectionToggle />);

    /* Detection ships on, so an unread store has to render on: "off" would
     * invite a click that turns working detection off. The row is disabled
     * until a status lands, which is what stops that click landing early. */
    expect(markup).toContain('role="switch" aria-checked="true"');
    expect(markup).toMatch(
      /<button(?=[^>]*role="switch")(?=[^>]*disabled="")[^>]*>/,
    );
  });

  test("automatic recording discloses the capture sources before consent", () => {
    const markup = paint(<MeetingAppsPicker />);

    expect(markup).toContain("captures your microphone and this Mac");
  });

  test("the auto-record switch is drawn only for the two apps whose grant is read", () => {
    const markup = paint(<MeetingAppsPicker />);

    /* The backend reads `detection_auto_record_apps` for a call app alone. A
     * switch on Zoom's row would round-trip, read back as on, and record
     * nothing: a silent failure the operator finds as a missing recording. */
    for (const id of ["facetime", "phone"]) {
      expect(markup).toContain('id="detection-auto-' + id + '"');
    }
    for (const id of ["zoom", "teams", "webex", "slack"]) {
      expect(markup).not.toContain('id="detection-auto-' + id + '"');
    }
  });

  test("the adopted-call line renders only when a call has been claimed", () => {
    /* MeetingDetectionSettings.tsx's `if (status.adoptedCall)` gate is the
     * only thing standing between an adopted call and the line naming it. A
     * flipped condition would print the sentence when there is nothing to
     * name, or stay silent while a call is stopping the capture.
     *
     * zustand hands React's *server* snapshot `getInitialState()`, so a
     * store seeded through `setState` never reaches `renderToStaticMarkup`
     * (WordCorrectionThreshold.test.tsx hit the same wall). Seeding the
     * snapshot object in place is what a server render actually reads. */
    const snapshot = useDetectionStore.getInitialState();
    const original = snapshot.status;
    const base: DetectionStatus = {
      eventSchemaVersion: 1,
      settings: {
        enabled: true,
        calendarEnabled: true,
        anyMicActivity: false,
        autoStartOnOpenPane: false,
        meetingApps: [],
        autoRecordApps: [],
      },
      calendarAccess: "authorized",
      notificationAccess: "authorized",
      inputDeviceActive: true,
      sonaHoldsInputDevice: false,
      suppressReason: null,
      countdown: null,
      adoptedCall: { bundleId: "com.apple.FaceTime", displayName: "FaceTime" },
      runningMeetingApps: [],
      inputDeviceReportingSuspect: false,
    };

    try {
      Object.assign(snapshot, { status: base });
      expect(paint(<MeetingDetectionState />)).toContain(
        "This recording stops when the FaceTime call ends.",
      );

      Object.assign(snapshot, { status: { ...base, adoptedCall: null } });
      expect(paint(<MeetingDetectionState />)).not.toContain(
        "This recording stops when the FaceTime call ends.",
      );
    } finally {
      Object.assign(snapshot, { status: original });
    }
  });
});

/* Payloads copied verbatim from the Rust serialization test
 * `the_prompt_wire_shape_names_variants_and_camelcases_fields` in
 * src-tauri/src/meeting/detection/machine.rs. `DetectionPromptKind` is no
 * longer a hand mirror — the prompt event is registered with the specta
 * builder, so bindings.ts generates the union from the Rust enum — but a
 * generated type only pins the shape the compiler sees, and these tests run on
 * bytes. When the two drifted, the pane rendered cards whose only content was
 * an app icon and two buttons; what failed then was the runtime titling, not
 * the declaration. So these strings stay: they prove `promptTitle` reads the
 * bytes Rust actually emits, which no type-check can reach. */
const WIRE_PAYLOADS: [string, string][] = [
  [
    '{"kind":"CalendarEvent","eventKey":"event-1","eventTitle":"Quarterly planning"}',
    "Quarterly planning",
  ],
  [
    '{"kind":"AppMeeting","bundleId":"us.zoom.xos","appName":"Zoom"}',
    "Zoom",
  ],
  [
    '{"kind":"AppHuddle","bundleId":"com.tinyspeck.slackmacgap","appName":"Slack"}',
    "Slack",
  ],
  [
    '{"kind":"BrowserCall","bundleId":"com.google.chrome","appName":"Chrome"}',
    "Chrome",
  ],
];

describe("wire shape", () => {
  for (const [payload, expected] of WIRE_PAYLOADS) {
    const kind = String(JSON.parse(payload).kind);
    test(`${kind} arrives as the union declares it`, () => {
      /* SAFETY: the payload bytes are pinned by the Rust wire-shape test
       * (the_prompt_wire_shape_names_variants_and_camelcases_fields), so this
       * cast states the contract under test rather than assuming one. */
      const prompt = JSON.parse(payload) as DetectionPromptKind;

      expect(promptTitle(i18n.t.bind(i18n), prompt)).toContain(expected);
    });
  }

});

/* The prompt list, driven the way the event listeners drive it. No React here:
 * zustand hands the server renderer its initial snapshot, so markup cannot
 * observe a seeded store — the list's own rules are what these check. */
const promptEvent = (
  promptId: string,
  prompt: DetectionPromptKind,
): DetectionPromptEvent => ({
  eventSchemaVersion: 1,
  promptId,
  prompt,
  notificationTitle: "unused by the in-app card",
  delivery: "notification",
  showIntroduction: false,
  announceInChat: false,
});

const status = (inputDeviceActive: boolean): DetectionStatus => ({
  eventSchemaVersion: 1,
  settings: {
    enabled: true,
    calendarEnabled: true,
    anyMicActivity: false,
    autoStartOnOpenPane: false,
    meetingApps: [],
    autoRecordApps: [],
  },
  calendarAccess: "authorized",
  notificationAccess: "authorized",
  inputDeviceActive,
  sonaHoldsInputDevice: false,
  suppressReason: null,
  countdown: null,
  adoptedCall: null,
  runningMeetingApps: [],
  inputDeviceReportingSuspect: false,
});

const zoom: DetectionPromptKind = {
  kind: "AppMeeting",
  bundleId: "us.zoom.xos",
  appName: "Zoom",
};

describe("the prompt list", () => {
  const seed = (...entries: DetectionPromptEvent[]) => {
    useDetectionStore.setState({ status: null, prompts: [] });
    for (const entry of entries) useDetectionStore.getState().addPrompt(entry);
    return useDetectionStore.getState();
  };

  test("one app across three microphone episodes is one offer", () => {
    /* What the operator actually saw. The backend mints a fresh prompt id per
     * raise and re-arms an app's claim every time the input device goes idle,
     * so three episodes in Zoom legitimately produce three ids for one
     * subject. Keyed by id alone they stacked into three identical cards. */
    const state = seed(
      promptEvent("uuid-1", zoom),
      promptEvent("uuid-2", zoom),
      promptEvent("uuid-3", zoom),
    );

    expect(state.prompts.map((entry) => entry.promptId)).toEqual(["uuid-3"]);
  });

  test("the newest raise is the one left standing", () => {
    const state = seed(
      promptEvent("uuid-1", { ...zoom, appName: "Zoom" }),
      promptEvent("uuid-2", { ...zoom, appName: "Zoom Workplace" }),
    );

    expect(promptTitle(i18n.t.bind(i18n), state.prompts[0].prompt)).toContain(
      "Zoom Workplace",
    );
  });

  test("different subjects are different offers", () => {
    const state = seed(
      promptEvent("uuid-1", zoom),
      promptEvent("uuid-2", {
        kind: "AppHuddle",
        bundleId: "com.tinyspeck.slackmacgap",
        appName: "Slack",
      }),
      promptEvent("uuid-3", {
        kind: "CalendarEvent",
        eventKey: "event-1",
        eventTitle: "Quarterly planning",
      }),
      promptEvent("uuid-4", { kind: "UnknownMicSource" }),
    );

    expect(state.prompts.map((entry) => entry.promptId)).toEqual([
      "uuid-1",
      "uuid-2",
      "uuid-3",
      "uuid-4",
    ]);
  });

  test("two calendar events are two offers, one event twice is one", () => {
    const event = (
      eventKey: string,
      eventTitle: string,
    ): DetectionPromptKind => ({
      kind: "CalendarEvent",
      eventKey,
      eventTitle,
    });
    const state = seed(
      promptEvent("uuid-1", event("event-1", "Quarterly planning")),
      promptEvent("uuid-2", event("event-2", "Design review")),
      promptEvent("uuid-3", event("event-1", "Quarterly planning")),
    );

    expect(state.prompts.map((entry) => entry.promptId)).toEqual([
      "uuid-2",
      "uuid-3",
    ]);
  });

  test("the microphone going idle retires the offers that depended on it", () => {
    seed(
      promptEvent("uuid-1", zoom),
      promptEvent("uuid-2", { kind: "UnknownMicSource" }),
      promptEvent("uuid-3", {
        kind: "CalendarEvent",
        eventKey: "event-1",
        eventTitle: "Quarterly planning",
      }),
    );

    useDetectionStore.getState().setStatus(status(false));

    /* The calendar prompt outlives the device deliberately: it is raised at
     * T-60s, before anyone has opened a microphone. */
    expect(
      useDetectionStore.getState().prompts.map((entry) => entry.promptId),
    ).toEqual(["uuid-3"]);
  });

  test("answering an offer clears only that one", () => {
    seed(
      promptEvent("uuid-1", zoom),
      promptEvent("uuid-2", { kind: "UnknownMicSource" }),
    );

    useDetectionStore.getState().clearPrompt("uuid-1");

    expect(
      useDetectionStore.getState().prompts.map((entry) => entry.promptId),
    ).toEqual(["uuid-2"]);
  });

});

/* The interleaving that reached the tree, and the gate that closes it.
 *
 * `detection_settings_set` takes the whole struct, so two overlapping writes
 * are two full overwrites — and the two rows that send them are adjacent on
 * Essentials. When the gate was a component-local `useState` and the base was
 * a render-old snapshot captured by the caller, this exact sequence ended with
 * `enabled: true`: a click on an app checkbox silently switched meeting
 * detection back on. The gate is store state now, which is what makes the race
 * expressible here at all — no React, no interaction harness.
 *
 * The host is faked at the Tauri boundary rather than by mocking a module, so
 * the store's own `invoke` runs and echoes back the settings it was handed,
 * which is what the real command answers. It resolves immediately: an async
 * function suspends at its first `await`, so a second call made before the
 * first resumes is exactly the click that arrives mid-save. */
describe("the shared write gate", () => {
  let sent: DetectionSettings[] = [];
  const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");

  beforeAll(() => {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: {
        ...globalThis.window,
        __TAURI_INTERNALS__: {
          invoke: (command: string, args: { settings: DetectionSettings }) => {
            if (command !== "detection_settings_set") {
              throw new Error(`unexpected command: ${command}`);
            }
            sent.push(args.settings);
            return Promise.resolve({
              ...status(true),
              settings: args.settings,
            });
          },
        },
      },
    });
  });
  afterAll(() => {
    if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
    else Reflect.deleteProperty(globalThis, "window");
  });

  const seed = () => {
    sent = [];
    useDetectionStore.setState({
      status: status(true),
      prompts: [],
      savingSettings: false,
    });
    return useDetectionStore.getState().patch;
  };

  test("an app box ticked mid-save cannot switch detection back on", async () => {
    const patch = seed();

    // The master switch, turned off. Its write is in flight.
    const off = patch({ enabled: false });

    /* The app picker one row below, ticking Zoom against the state the store
     * still reports — `enabled: true`, because the write has not landed. It
     * shares the switch's gate, so nothing is sent. */
    void patch({ meetingApps: ["us.zoom.xos"] });
    expect(sent).toHaveLength(1);

    await off;

    expect(useDetectionStore.getState().status?.settings.enabled).toBe(false);
  });

  test("a row a render behind still writes off the landed base", async () => {
    const patch = seed();

    await patch({ enabled: false });

    /* Whatever the picker believes about `enabled` is a render old by now. The
     * base is read at call time, so its write carries the one field it changed
     * and inherits the rest from what actually landed. */
    await patch({ meetingApps: ["us.zoom.xos"] });

    expect(sent[1].enabled).toBe(false);
    expect(sent[1].meetingApps).toEqual(["us.zoom.xos"]);
    expect(useDetectionStore.getState().status?.settings.enabled).toBe(false);
  });

  test("a write with no status to write onto is not sent", async () => {
    const patch = seed();
    useDetectionStore.setState({ status: null });

    await patch({ enabled: false });

    expect(sent).toHaveLength(0);
  });

  /* The one setting that turns a notice into a recording. It has to survive the
   * whole-struct write intact, and it has to leave the rest of the struct
   * alone. */
  test("a standing grant round-trips without disturbing the allowlist", async () => {
    const patch = seed();

    await patch({ meetingApps: ["com.apple.facetime"] });
    await patch({ autoRecordApps: ["com.apple.facetime"] });

    expect(sent[1].meetingApps).toEqual(["com.apple.facetime"]);
    expect(sent[1].autoRecordApps).toEqual(["com.apple.facetime"]);
    expect(
      useDetectionStore.getState().status?.settings.autoRecordApps,
    ).toEqual(["com.apple.facetime"]);

    await patch({ autoRecordApps: [] });

    expect(sent[2].meetingApps).toEqual(["com.apple.facetime"]);
    expect(
      useDetectionStore.getState().status?.settings.autoRecordApps,
    ).toEqual([]);
  });
});

/* The decided-permission case: macOS answers an enable attempt without a
 * dialog and without full access. The store must say it happened, because
 * nothing on screen otherwise moves. */
describe("a refused calendar enable", () => {
  const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");

  /* The backend never turned the path on, so the status it hands back after
   * the refused request still says the calendar is off. */
  const refusedStatus = (): DetectionStatus => {
    const base = status(true);
    return { ...base, settings: { ...base.settings, calendarEnabled: false } };
  };

  beforeAll(() => {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: {
        ...globalThis.window,
        __TAURI_INTERNALS__: {
          invoke: (command: string) => {
            if (command === "detection_calendar_access_request") {
              return Promise.resolve("denied");
            }
            if (command === "detection_status_get") {
              return Promise.resolve(refusedStatus());
            }
            if (command === "detection_settings_set") {
              return Promise.resolve(refusedStatus());
            }
            throw new Error(`unexpected command: ${command}`);
          },
        },
      },
    });
  });
  afterAll(() => {
    if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
    else Reflect.deleteProperty(globalThis, "window");
  });

  test("sets the flag, writes nothing, and turning off clears it", async () => {
    const sent: DetectionSettings[] = [];
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: {
        ...globalThis.window,
        __TAURI_INTERNALS__: {
          invoke: (command: string, args: { settings?: DetectionSettings }) => {
            if (command === "detection_calendar_access_request") {
              return Promise.resolve("denied");
            }
            if (command === "detection_status_get") {
              return Promise.resolve(refusedStatus());
            }
            if (command === "detection_settings_set") {
              if (args.settings) {
                sent.push(args.settings);
              }
              return Promise.resolve({
                ...refusedStatus(),
                settings: args.settings,
              });
            }
            throw new Error(`unexpected command: ${command}`);
          },
        },
      },
    });

    useDetectionStore.setState({
      status: refusedStatus(),
      prompts: [],
      savingSettings: false,
      calendarRefused: false,
    });

    await useDetectionStore.getState().enableCalendar(true);
    expect(useDetectionStore.getState().calendarRefused).toBe(true);
    expect(sent).toEqual([]);

    await useDetectionStore.getState().enableCalendar(false);
    expect(useDetectionStore.getState().calendarRefused).toBe(false);
    expect(sent).toHaveLength(1);
    expect(sent[0]?.calendarEnabled).toBe(false);
  });
});
