import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import {
  deriveElapsedSeconds,
  deriveHudFrame,
  deriveHudPhase,
  hudCaptureReady,
  hudFailed,
  hudHidden,
  hudRested,
  hudShown,
  hudStreamPhaseChanged,
  hudTicked,
  INITIAL_HUD_STATE,
  type HudFrame,
  type HudPhase,
} from "./hudMachine";
import { RecordingOverlayContent } from "./RecordingOverlayContent";

/* The HUD is the surface a user watches while they are speaking, so the thing
 * under test is that its five states are each unmistakable — and specifically
 * that `starting` can never be read as `listening`. The microphone stream takes
 * 140-215 ms to open (unbounded on Bluetooth) and the overlay is on screen for
 * all of it; a user who talks into that window loses the head of the utterance.
 *
 * The row is two readouts: the state word and the elapsed clock. Both are
 * text, so what is asserted here is the text and the phase it belongs to.
 * Theme and material are root attributes resolved in CSS, so every colour
 * arrives through a `hud-<phase>` token hook and no state hardcodes one; the
 * rendered appearance in dark/light x solid/glass is screenshot work. */

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
  elapsedSeconds?: number | null;
  error?: { error_type: string; detail?: string };
  frame?: HudFrame;
}

const render = ({
  hud,
  elapsedSeconds = null,
  error = undefined,
  frame = "compact",
}: HudCase): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <RecordingOverlayContent
        isVisible
        hud={hud}
        frame={frame}
        elapsedSeconds={elapsedSeconds}
        streamText={{ committed: "", tentative: "" }}
        modeName="Email"
        error={error ?? null}
        session={1}
        position="bottom"
        direction="ltr"
        capRef={{ current: null }}
        onStreamScroll={() => {}}
      />
    </I18nextProvider>,
  );

describe("the HUD state machine", () => {
  test("withholds listening until the recorder reports its first buffer", () => {
    const shown = hudShown(INITIAL_HUD_STATE, "recording");
    expect(deriveHudPhase(shown)).toBe("starting");
    expect(deriveElapsedSeconds(shown)).toBe(null);

    const live = hudCaptureReady(shown, 1_000);
    expect(deriveHudPhase(live)).toBe("listening");
    expect(deriveElapsedSeconds({ ...live, nowMs: 8_000 })).toBe(7);
  });

  test("a fresh show re-arms readiness, so a second run cannot inherit it", () => {
    const live = hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording"), 1);
    const again = hudShown(live, "recording");
    expect(again.captureReady).toBe(false);
    expect(again.readyAt).toBe(null);
    expect(deriveHudPhase(again)).toBe("starting");
  });

  test("splits the streaming card's working phase by its work kind", () => {
    const live = hudCaptureReady(hudShown(INITIAL_HUD_STATE, "streaming"), 1);
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

  test("the elapsed readout freezes at the capture's real end", () => {
    const live = hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording"), 1);
    const stopped = hudShown(live, "transcribing");
    expect(stopped.nowMs >= live.nowMs).toBe(true);
    // Frozen: the clock's origin survives, so the number stops moving.
    expect(stopped.readyAt).toBe(1);
  });

  /* The overlay schedules its one-second tick only while the microphone is
   * open, but clearing that interval races the state change that closed the
   * capture, so a tick can always land one beat late. The frozen capture
   * length is a measurement: a late tick must not walk it forward into a
   * number the recorder never reported. */
  test("a tick after the capture stopped leaves the frozen readout alone", () => {
    const live = hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording"), 1);
    const stopped = hudShown(live, "transcribing");
    expect(hudTicked(stopped)).toBe(stopped);
    expect(
      hudTicked(hudFailed(live, { error_type: "capture_overrun" })).nowMs,
    ).toBe(hudFailed(live, { error_type: "capture_overrun" }).nowMs);
  });

  test("a tick before the first buffer has no origin to count from", () => {
    const starting = hudShown(INITIAL_HUD_STATE, "recording");
    expect(hudTicked(starting)).toBe(starting);
    expect(deriveElapsedSeconds(hudTicked(starting))).toBe(null);
  });

  test("a tick while the microphone is open advances the readout", () => {
    const live = hudCaptureReady(
      hudShown(INITIAL_HUD_STATE, "recording"),
      Date.now() - 5_000,
    );
    expect(deriveElapsedSeconds(live)).toBe(0);
    expect(deriveElapsedSeconds(hudTicked(live))).toBeGreaterThan(4);
  });

  /* actions.rs emits `recording-error` and calls `hide_recording_overlay` back
   * to back, so both land in the same tick. A hide must therefore record where
   * to rest rather than tear an unread failure off the screen. */
  test("the hide that follows a failure does not tear the failure down", () => {
    const failed = hudFailed(
      hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording"), 1),
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
  });

  test("a new dictation retires a failure the user never read", () => {
    const failed = hudFailed(hudShown(INITIAL_HUD_STATE, "recording"), {
      error_type: "capture_overrun",
    });
    expect(deriveHudPhase(hudShown(failed, "recording"))).toBe("starting");
  });

  test("an ordinary hide clears the capture without leaving a clock behind", () => {
    const hidden = hudHidden(
      hudCaptureReady(hudShown(INITIAL_HUD_STATE, "recording"), 1),
    );
    expect(hidden.isVisible).toBe(false);
    expect(deriveElapsedSeconds(hidden)).toBe(null);
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

describe("the compact HUD is a state word and a clock", () => {
  test("the row is those two readouts and nothing else", () => {
    const markup = render({ hud: "listening", elapsedSeconds: 42 });
    expect(markup).toContain("Listening");
    expect(markup).toContain("0:42");
    /* Everything the row used to carry: sixteen animated meter bars that asked
     * a sighted reader to infer the state from a travelling opacity crest, the
     * app mark beside them, and — before those — the mode, the engine, the
     * chord and a hint. The word and the number are the whole instrument. */
    const gone = [
      "smark",
      "swave",
      "--bar-index",
      "sr-only",
      "Input level",
      "Email",
      "Cloud",
      "<kbd",
      "smode",
      "sengine",
      "shint",
      "sring",
    ];
    expect(gone.filter((token) => markup.includes(token))).toEqual([]);
  });

  for (const [hud, word] of [
    ["starting", "Starting"],
    ["listening", "Listening"],
    ["transcribing", "Transcribing"],
    ["processing", "Processing"],
  ] as const) {
    test(`${hud} says its own word out loud`, () => {
      const markup = render({ hud });
      expect(markup).toContain(`>${word}<`);
      // A word that changes under a reader's eyes is announced, not swapped.
      expect(markup).toContain('aria-live="polite"');
    });
  }

  /* The clock measures captured audio, so it may never claim a second the
   * recorder has not finished. Flooring is what guarantees that. */
  for (const [seconds, readout] of [
    [0.9, "0:00"],
    [59.9, "0:59"],
    [60, "1:00"],
    [671.5, "11:11"],
  ] as const) {
    test(`${seconds} captured seconds reads ${readout}`, () => {
      expect(render({ hud: "listening", elapsedSeconds: seconds })).toContain(
        readout,
      );
    });
  }

  test("there is no clock until the microphone is actually live", () => {
    const starting = render({ hud: "starting" });
    expect(starting).toContain("Starting");
    expect(starting.includes("stime")).toBe(false);
    // The number that does arrive opts out of every transition and keyframe.
    expect(render({ hud: "listening", elapsedSeconds: 3 })).toContain(
      "stime snap-measured",
    );
  });

  test("a failure names the cause in the app's own words, not its token", () => {
    const markup = render({
      hud: "error",
      elapsedSeconds: 12,
      error: { error_type: "no_speech_detected" },
    });
    expect(markup).toContain("hud-error");
    expect(markup).toContain("No speech detected");
    expect(markup.includes("no_speech_detected")).toBe(false);
    // Nothing to cancel once the run is over, and no elapsed time left to read.
    expect(markup.includes('class="sx"')).toBe(false);
    expect(markup.includes("stime")).toBe(false);
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

  /* Cancel is the only control on the row. It is absolutely positioned and
   * transparent until the row is hovered, so at rest the row is the two
   * readouts — but it is a real, labelled button in the markup the whole time,
   * not something conjured on hover.
   *
   * The reveal itself is CSS and the click path runs through the Tauri command
   * bridge, so both are verified by driving the overlay in a browser rather
   * than here: this repo renders every test to static markup and has no DOM. */
  for (const hud of [
    "starting",
    "listening",
    "transcribing",
    "processing",
  ] as const) {
    test(`${hud} keeps cancel as a real labelled button`, () => {
      const markup = render({ hud, elapsedSeconds: 5 });
      expect(markup).toContain('<button class="sx"');
      expect(markup).toContain('aria-label="Cancel"');
    });
  }

  test("idle renders the pill instead of an instrument row", () => {
    const markup = render({ hud: "idle", frame: "pill" });
    expect(markup).toContain('data-testid="hud-pill"');
    // The pill is the one surface that still names the mode: it is a switcher.
    expect(markup).toContain("Email");
    expect(markup.includes("sbase")).toBe(false);
    expect(markup.includes("smark")).toBe(false);
  });

  /* The resting window is 176x28. The instrument row drawn there would be
   * clipped by the window, so the failure takes the pill's own shape. */
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
    expect(markup.includes("smark")).toBe(false);
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
        elapsedSeconds: 7,
        error: { error_type: "capture_overrun" },
      });
      expect(markup).toContain(`hud-${hud}`);
      expect(markup.includes("color:")).toBe(false);
    });
  }
});

describe("the Live panel shares the instrument row", () => {
  test("it keeps the streaming text region and the same two readouts", () => {
    const markup = renderToStaticMarkup(
      <I18nextProvider i18n={i18n}>
        <RecordingOverlayContent
          isVisible
          hud="listening"
          frame="stream"
          elapsedSeconds={90}
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
    expect(markup).toContain("sbase");
    expect(markup).toContain("Listening");
    expect(markup).toContain("1:30");
    // The row carries the same two things it carries in the compact window.
    const gone = ["swave", "smark", "Email", "Cloud", "<kbd"];
    expect(gone.filter((token) => markup.includes(token))).toEqual([]);
  });
});
