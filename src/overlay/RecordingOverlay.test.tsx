import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { LanguageDirection } from "@/lib/utils/rtl";
import {
  deriveHudFrame,
  deriveHudPhase,
  hudCaptureReady,
  hudFailed,
  hudHidden,
  hudLevelChanged,
  hudRested,
  hudShown,
  hudStreamPhaseChanged,
  INITIAL_HUD_STATE,
  type HudFrame,
  type HudPhase,
} from "./hudMachine";
import { RecordingOverlayContent } from "./RecordingOverlayContent";

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
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

interface HudCase {
  hud: HudPhase;
  levels?: readonly number[];
  error?: { error_type: string; detail?: string };
  frame?: HudFrame;
  direction?: LanguageDirection;
}

const render = ({
  hud,
  levels = [],
  error = undefined,
  frame = "compact",
  direction = "ltr",
}: HudCase): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <RecordingOverlayContent
        isVisible
        hud={hud}
        frame={frame}
        levels={levels}
        streamText={{ committed: "", tentative: "" }}
        modeName="Email"
        error={error ?? null}
        session={1}
        position="bottom"
        direction={direction}
        capRef={{ current: null }}
        onStreamScroll={() => {}}
      />
    </I18nextProvider>,
  );

const meterScales = (markup: string): number[] => {
  const meterStart = markup.indexOf('role="img" aria-label="Input level"');
  if (meterStart === -1) return [];
  return [...markup.slice(meterStart).matchAll(/scaleY\(([^)]+)\)/g)].map(
    (match) => Number(match[1]),
  );
};

const LEVELS = [
  0.08, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.85, 0.9, 0.92, 0.94, 0.96,
  0.98, 1,
];

describe("the HUD state machine", () => {
  test("withholds level frames until the visible capture is ready", () => {
    const starting = hudShown(INITIAL_HUD_STATE, "recording");
    expect(hudLevelChanged(starting, LEVELS)).toBe(starting);

    const listening = hudCaptureReady(starting);
    const updated = hudLevelChanged(listening, LEVELS);
    expect(deriveHudPhase(updated)).toBe("listening");
    expect(updated.levels).toEqual(LEVELS);
  });

  test("a fresh show re-arms readiness and drops the previous frame", () => {
    const listening = hudLevelChanged(
      hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording")),
      LEVELS,
    );
    const again = hudShown(listening, "recording");

    expect(again.captureReady).toBe(false);
    expect(again.levels).toEqual([]);
    expect(deriveHudPhase(again)).toBe("starting");
  });

  test("a streaming work transition ignores later level frames", () => {
    const listening = hudLevelChanged(
      hudCaptureReady(hudShown(INITIAL_HUD_STATE, "streaming")),
      LEVELS,
    );
    const working = hudStreamPhaseChanged(listening, { phase: "working" });

    expect(working.levels).toEqual([]);
    expect(hudLevelChanged(working, LEVELS)).toBe(working);
    expect(deriveHudPhase(working)).toBe("transcribing");
  });

  test("error, hide, rest, idle, and work states clear levels", () => {
    const listening = hudLevelChanged(
      hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording")),
      LEVELS,
    );
    const failed = hudFailed(listening, { error_type: "capture_overrun" });

    expect(failed.levels).toEqual([]);
    expect(hudShown(listening, "transcribing").levels).toEqual([]);
    expect(hudShown(listening, "processing").levels).toEqual([]);
    expect(hudShown(listening, "idle").levels).toEqual([]);
    expect(hudHidden(listening).levels).toEqual([]);
    expect(hudRested(hudHidden(failed)).levels).toEqual([]);
  });

  test("splits the streaming card's working phase by its work kind", () => {
    const live = hudCaptureReady(hudShown(INITIAL_HUD_STATE, "streaming"));
    expect(deriveHudPhase(live)).toBe("listening");
    expect(
      deriveHudPhase(hudStreamPhaseChanged(live, { phase: "working" })),
    ).toBe("transcribing");
    expect(
      deriveHudPhase(
        hudStreamPhaseChanged(live, { phase: "working", kind: "polishing" }),
      ),
    ).toBe("processing");
  });

  test("maps the compact backend states onto transcribing and processing", () => {
    expect(deriveHudPhase(hudShown(INITIAL_HUD_STATE, "transcribing"))).toBe(
      "transcribing",
    );
    expect(deriveHudPhase(hudShown(INITIAL_HUD_STATE, "processing"))).toBe(
      "processing",
    );
  });

  /* actions.rs emits `recording-error` and calls `hide_recording_overlay` back
   * to back, so a hide must let the failure dwell before the window rests. */
  test("the hide that follows a failure does not tear the failure down", () => {
    const failed = hudFailed(
      hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording")),
      { error_type: "no_speech_detected" },
    );
    expect(deriveHudPhase(failed)).toBe("error");

    const hidden = hudHidden(failed);
    expect(hidden.isVisible).toBe(true);
    expect(hidden.restAfterError).toBe("hide");
    expect(deriveHudPhase(hidden)).toBe("error");

    const rested = hudRested(hidden);
    expect(rested.isVisible).toBe(false);
    expect(rested.error).toBe(null);
    expect(rested.levels).toEqual([]);
  });

  test("resting into the pill holds the failure, then reveals the pill", () => {
    const failed = hudFailed(hudShown(INITIAL_HUD_STATE, "recording"), {
      error_type: "capture_overrun",
    });
    const resting = hudShown(failed, "idle");
    expect(deriveHudPhase(resting)).toBe("error");
    expect(deriveHudFrame(resting)).toBe("pill");

    const rested = hudRested(resting);
    expect(rested.isVisible).toBe(true);
    expect(deriveHudPhase(rested)).toBe("idle");
    expect(rested.levels).toEqual([]);
  });

  test("a new dictation retires a failure the user never read", () => {
    const failed = hudFailed(hudShown(INITIAL_HUD_STATE, "recording"), {
      error_type: "capture_overrun",
    });
    expect(deriveHudPhase(hudShown(failed, "recording"))).toBe("starting");
  });

  test("an ordinary hide clears the capture and its last frame", () => {
    const hidden = hudHidden(
      hudLevelChanged(
        hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording")),
        LEVELS,
      ),
    );
    expect(hidden.isVisible).toBe(false);
    expect(hidden.captureReady).toBe(false);
    expect(hidden.levels).toEqual([]);
  });

  test("each backend state names the window it is drawn into", () => {
    expect(deriveHudFrame(hudShown(INITIAL_HUD_STATE, "recording"))).toBe(
      "compact",
    );
    expect(deriveHudFrame(hudShown(INITIAL_HUD_STATE, "streaming"))).toBe(
      "stream",
    );
    expect(deriveHudFrame(hudShown(INITIAL_HUD_STATE, "idle"))).toBe("pill");
  });
});

describe("the listening HUD is the measured input meter", () => {
  test("sixteen zero buckets render sixteen visible baseline bars", () => {
    const markup = render({ hud: "listening", levels: Array(16).fill(0) });

    expect(meterScales(markup)).toEqual(Array(16).fill(0.06));
    expect(markup).toContain('role="img" aria-label="Input level"');
    expect(markup).toContain(
      'role="status" aria-live="polite" aria-atomic="true">Listening</span>',
    );
    expect(markup).not.toContain(">0:00<");
  });

  test("mixed buckets render producer values monotonically without a second curve", () => {
    const markup = render({ hud: "listening", levels: LEVELS });
    const scales = meterScales(markup);

    expect(scales).toHaveLength(16);
    expect(scales[0]).toBeGreaterThan(0.06);
    expect(scales[1]).toBeGreaterThan(scales[0]);
    expect(scales[2]).toBeGreaterThan(scales[1]);
    expect(scales[3]).toBeGreaterThan(scales[2]);
    expect(scales[4]).toBeGreaterThan(scales[3]);
    expect(scales[5]).toBeGreaterThan(scales[4]);
    expect(scales[15]).toBe(1);
    expect(scales).toEqual(LEVELS);
    expect(
      meterScales(render({ hud: "listening", levels: Array(16).fill(1) })),
    ).toEqual(Array(16).fill(1));
  });

  test("the established speech threshold marks a reported peak", () => {
    const quietLevels = Array(16).fill(0.89);
    const speechLevels = Array(16).fill(0.9);
    const quiet = render({ hud: "listening", levels: quietLevels });
    const speech = render({ hud: "listening", levels: speechLevels });

    expect(meterScales(quiet)).toEqual(quietLevels);
    expect(meterScales(speech)).toEqual(speechLevels);
  });
  test("compact and stream frames preserve the requested direction", () => {
    const compact = render({ hud: "listening", direction: "rtl" });
    const stream = render({
      hud: "listening",
      frame: "stream",
      direction: "rtl",
    });

    expect(
      [compact, stream].map((markup) => markup.includes('dir="rtl"')),
    ).toEqual([true, true]);
  });

  for (const [hud, word] of [
    ["starting", "Starting"],
    ["transcribing", "Transcribing"],
    ["processing", "Processing"],
  ] as const) {
    test(`${hud} keeps its visible localized status`, () => {
      const markup = render({ hud });
      expect(markup).toContain(`>${word}<`);
      expect(markup).toContain('aria-live="polite"');
      expect(markup).not.toContain("swave");
    });
  }
});

describe("the compact HUD preserves failure and cancel behavior", () => {
  test("a failure names the cause in the app's own words, not its token", () => {
    const markup = render({
      hud: "error",
      error: { error_type: "no_speech_detected" },
    });
    expect(markup).toContain("hud-error");
    expect(markup).toContain("No speech detected");
    expect(markup.includes("no_speech_detected")).toBe(false);
    expect(markup.includes('class="sx"')).toBe(false);
    expect(markup.includes("swave")).toBe(false);
  });

  for (const [errorType, cause] of [
    ["microphone_permission_denied", "Microphone access denied"],
    ["no_input_device", "No microphone found"],
    ["no_model_selected", "No model selected"],
    ["no_speech_save_failed", "Sample not saved"],
    ["capture_overrun", "Recording cut short"],
    ["cloud_unavailable", "Cloud unavailable"],
    ["cloud_transcription_held", "Cloud result held"],
    ["command_no_selection", "Nothing selected"],
    ["command_rewrite_unavailable", "Rewrite unavailable"],
  ] as const) {
    test(`${errorType} reads as its own cause`, () => {
      const markup = render({ hud: "error", error: { error_type: errorType } });
      expect(markup).toContain(cause);
      expect(markup.includes(errorType)).toBe(false);
    });
  }

  test("an unmapped cause summarises instead of leaking the token", () => {
    const markup = render({
      hud: "error",
      error: { error_type: "some_future_error", detail: "raw backend detail" },
    });
    expect(markup).toContain("Failed");
    expect(markup.includes("some_future_error")).toBe(false);
    expect(markup.includes("raw backend detail")).toBe(false);
  });

  for (const hud of [
    "starting",
    "listening",
    "transcribing",
    "processing",
  ] as const) {
    test(`${hud} keeps cancel as a real labelled button`, () => {
      const markup = render({ hud });
      expect(markup).toContain('<button class="sx"');
      expect(markup).toContain('aria-label="Cancel"');
    });
  }

  test("idle renders the pill instead of an instrument row", () => {
    const markup = render({ hud: "idle", frame: "pill" });
    expect(markup).toContain('data-testid="hud-pill"');
    expect(markup).toContain("Email");
    expect(markup.includes("sbase")).toBe(false);
  });

  test("a failure in the resting window renders as a one-line pill", () => {
    const markup = render({
      hud: "error",
      frame: "pill",
      error: { error_type: "no_speech_detected" },
    });
    expect(markup).toContain('data-testid="hud-error-pill"');
    expect(markup).toContain("hud-pill hud-error");
    expect(markup).toContain("No speech detected");
    expect(markup.includes("sbase")).toBe(false);
  });

  for (const hud of [
    "starting",
    "listening",
    "transcribing",
    "processing",
    "error",
  ] as const) {
    test(`${hud} hooks its colour on a phase class, never inline`, () => {
      const markup = render({
        hud,
        error: { error_type: "capture_overrun" },
      });
      expect(markup).toContain(`hud-${hud}`);
      expect(markup.includes("color:")).toBe(false);
    });
  }
});

describe("the Live panel shares the measured meter", () => {
  test("it keeps streaming text and the same listening meter", () => {
    const markup = renderToStaticMarkup(
      <I18nextProvider i18n={i18n}>
        <RecordingOverlayContent
          isVisible
          hud="listening"
          frame="stream"
          levels={LEVELS}
          streamText={{ committed: "Hello", tentative: " world" }}
          modeName="Email"
          error={null}
          session={1}
          position="bottom"
          direction="ltr"
          capRef={{ current: null }}
          onStreamScroll={() => {}}
        />
      </I18nextProvider>,
    );
    expect(markup).toContain("stext-cap");
    expect(markup).toContain("Hello");
    expect(markup).toContain("scard hud-listening open");
    expect(markup).toContain('role="img" aria-label="Input level"');
    expect(meterScales(markup)).toHaveLength(16);
    expect(markup).not.toContain(">0:00<");
  });
});
