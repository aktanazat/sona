import type { RefObject } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import type { StreamTextEvent } from "@/bindings";
import type { LanguageDirection } from "@/lib/utils/rtl";
import type { OverlayPosition } from "@/lib/powerPackApi";
import type { RecordingErrorEvent } from "@/lib/types/events";
import type { HudFrame, HudPhase } from "./hudMachine";
import { HudPill } from "./HudPill";

interface RecordingOverlayContentProps {
  isVisible: boolean;
  hud: HudPhase;
  /**
   * The window the HUD is being drawn into. `pill` is 184x36 and holds the idle
   * mode switcher; `compact` and `stream` hold the instrument row — the state
   * word on the leading edge, the elapsed clock on the trailing one.
   */
  frame: HudFrame;
  /** Seconds of captured audio; null before the microphone is live. */
  elapsedSeconds: number | null;
  streamText: StreamTextEvent;
  modeName: string | null;
  error: RecordingErrorEvent | null;
  session: number;
  position: OverlayPosition;
  direction: LanguageDirection;
  /* React 19: `useRef<HTMLDivElement>(null)` yields `RefObject<T | null>`, so
   * the null is part of the type rather than something the caller asserts away. */
  capRef: RefObject<HTMLDivElement | null>;
  onStreamScroll: () => void;
}

/**
 * Every `error_type` `actions.rs` and `command_mode.rs` emit, mapped to the
 * short `errors.*Title` string for that condition. The three that already had a
 * title are reused as-is — App.tsx toasts the same keys — and the rest follow
 * the file's existing `<condition>Title` + `<condition>` sentence convention, so
 * a toast can adopt them later without a second string appearing.
 *
 * An unlisted cause deliberately renders the plain "Failed" summary rather than
 * its token.
 */
const ERROR_TITLE_KEYS = {
  no_speech_detected: "errors.noSpeechDetectedTitle",
  microphone_permission_denied: "errors.micPermissionDeniedTitle",
  no_input_device: "errors.noInputDeviceTitle",
  no_model_selected: "errors.noModelSelectedTitle",
  no_speech_save_failed: "errors.noSpeechSaveFailedTitle",
  capture_overrun: "errors.captureOverrunTitle",
  cloud_unavailable: "errors.cloudUnavailableTitle",
  cloud_transcription_held: "errors.cloudTranscriptionHeldTitle",
  command_no_selection: "errors.commandNoSelectionTitle",
  command_rewrite_unavailable: "errors.commandRewriteUnavailableTitle",
} satisfies Record<string, string>;

/** The short title for one emitted `error_type`, or undefined when the app has
 * no words for that cause yet. */
const errorTitleKey = (token: string): string | undefined => {
  if (!(token in ERROR_TITLE_KEYS)) return undefined;
  /* SAFETY: the `in` check just established that the token is one of this
     object's own keys, which is exactly what the index needs. */
  return ERROR_TITLE_KEYS[token as keyof typeof ERROR_TITLE_KEYS];
};

/**
 * m:ss, floored. A readout may never claim a second the recorder has not
 * finished, so the partial second is dropped rather than rounded up.
 */
const clock = (seconds: number): string => {
  const whole = Math.max(0, Math.floor(seconds));
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
};

export const RecordingOverlayContent = ({
  isVisible,
  hud,
  frame,
  elapsedSeconds,
  streamText,
  modeName,
  error,
  session,
  position,
  direction,
  capRef,
  onStreamScroll,
}: RecordingOverlayContentProps) => {
  const { t } = useTranslation();

  if (!isVisible) return null;

  /* The state word, on screen. It used to be visually hidden while sixteen
   * animated bars carried the state to sighted readers, which asked them to
   * infer "the transcriber is working" from a travelling opacity crest. One
   * word answers it, in the app's own vocabulary, for everyone at once. */
  const stateLabel = {
    idle: t("overlay.hud.idle", "Ready"),
    starting: t("overlay.state.starting", "Starting"),
    listening: t("overlay.state.listening", "Listening"),
    transcribing: t("overlay.state.transcribing", "Transcribing"),
    processing: t("overlay.state.processing", "Processing"),
    error: t("overlay.state.failed", "Failed"),
  }[hud];

  /* A failure states the cause, in the app's own words for that cause. The
   * `error_type` values are semantic (`no_model_selected`, `capture_overrun`),
   * so the cause is known — but a snake_case token on screen is the opacity the
   * "no apology copy, say exactly what failed" rule exists to ban. These are the
   * same conditions App.tsx toasts, keyed to the short `errors.*Title` form of
   * each, so the HUD and the toast cannot drift. A cause with no short form
   * falls back to "Failed": an honest summary, where the token is not. */
  const errorLabelKey = error && errorTitleKey(error.error_type);
  const failureText = (errorLabelKey && t(errorLabelKey)) || stateLabel;

  /* The resting window holds 176x28. A failure that lands after the backend has
   * already rested the overlay to its pill therefore renders as the pill: the
   * same shell, with the cause where the mode name was. */
  if (frame === "pill") {
    if (hud === "error") {
      return (
        <div dir={direction} className={`ov-stage ${position} ov-fade show`}>
          <div
            className="scard compact hud-pill hud-error"
            data-testid="hud-error-pill"
          >
            <span className="serror" role="alert">
              {failureText}
            </span>
          </div>
        </div>
      );
    }
    return (
      <HudPill position={position} direction={direction} modeName={modeName} />
    );
  }

  const failed = hud === "error";

  /* Two things, one line: what the HUD is doing, and how long it has been
   * capturing. A failure replaces both with its cause — there is no elapsed
   * time worth reading once the run is over. */
  const instrumentRow = (
    <div className="sbase">
      {failed ? (
        <span className="serror" role="alert">
          {failureText}
        </span>
      ) : (
        <>
          <span
            className="sstate"
            role="status"
            aria-live="polite"
            aria-atomic="true"
          >
            {stateLabel}
          </span>
          {/* The clock is a measurement: it is stamped from the recorder's own
              first-buffer time, frozen at the real end of the capture, and
              opted out of every transition and keyframe so it can only ever
              show a second that actually elapsed. Left out entirely until the
              microphone is live, because there is nothing to count yet. */}
          {elapsedSeconds !== null && (
            <span className="stime snap-measured">{clock(elapsedSeconds)}</span>
          )}
        </>
      )}
      {/* Nothing to cancel once the run has failed. The button is the one thing
          the pill hides until it is asked for: it rides the trailing end of the
          row on hover, so at rest the row is the two readouts and nothing else.

          Hover is the only way to reach it. The overlay is a nonactivating
          panel that never takes keyboard focus (overlay.rs: focusable(false),
          can_become_key_window false), which is what keeps the app being
          dictated into in front, so the :focus-visible rule beside the hover
          one in RecordingOverlay.css cannot fire here. Cancelling by keyboard
          is the shortcut's job. */}
      {!failed && (
        <button
          className="sx"
          aria-label={t("common.cancel")}
          onClick={() => commands.cancelOperation()}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path
              d="M4 4 L12 12 M12 4 L4 12"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
            />
          </svg>
        </button>
      )}
    </div>
  );

  /* Solid in both materials, by ruling: the words on this card are what the
   * instrument exists to show, and a tint that lets unblurred wallpaper through
   * contests exactly that. Glass lives on the idle pill only, where nothing
   * being reported is drawn. */
  if (frame === "stream") {
    const hasText =
      streamText.committed.length > 0 || streamText.tentative.length > 0;

    return (
      <div dir={direction} className={`ov-stage ${position}`}>
        <div
          key={session}
          className={["scard", `hud-${hud}`, hasText && "open"]
            .filter(Boolean)
            .join(" ")}
          data-testid="hud-card"
        >
          <div className="stext">
            <div className="stext-clip">
              <div className="stext-cap" ref={capRef} onScroll={onStreamScroll}>
                <p>
                  <span className="committed">
                    {streamText.committed ? `${streamText.committed} ` : ""}
                  </span>
                  <span className="tentative">{streamText.tentative}</span>
                </p>
              </div>
            </div>
          </div>
          {instrumentRow}
        </div>
      </div>
    );
  }

  return (
    <div dir={direction} className={`ov-stage ${position} ov-fade show`}>
      <div className={`scard compact hud-${hud}`} data-testid="hud-card">
        {instrumentRow}
      </div>
    </div>
  );
};
