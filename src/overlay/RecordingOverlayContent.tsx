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
  /** The window the HUD is being drawn into. */
  frame: HudFrame;
  /** The most recent sixteen normalized microphone buckets. */
  levels: readonly number[];
  streamText: StreamTextEvent;
  modeName: string | null;
  error: RecordingErrorEvent | null;
  session: number;
  position: OverlayPosition;
  direction: LanguageDirection;
  /* React 19: useRef<HTMLDivElement>(null) yields RefObject<T | null>. */
  capRef: RefObject<HTMLDivElement | null>;
  onStreamScroll: () => void;
}

/**
 * Every error_type emitted by actions.rs and command_mode.rs maps to the short
 * title already used by the main-window toast. Unknown causes use Failed rather
 * than exposing a backend token on the overlay.
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

const errorTitleKey = (token: string): string | undefined => {
  if (!(token in ERROR_TITLE_KEYS)) return undefined;
  /* SAFETY: the in check above establishes that token is one of this
     object's own keys, which is exactly what the indexed lookup needs. */
  return ERROR_TITLE_KEYS[token as keyof typeof ERROR_TITLE_KEYS];
};

const METER_BAR_COUNT = 16;
const RESTING_METER_BARS: readonly number[] = Array(METER_BAR_COUNT).fill(0);
const SPEECH_PEAK = 0.9;

const barScale = (level: number): number =>
  Math.max(0.06, Math.min(1, Math.max(0, level)));

const hearingSpeech = (levels: readonly number[]): boolean =>
  levels.some((level) => level >= SPEECH_PEAK);

export const RecordingOverlayContent = ({
  isVisible,
  hud,
  frame,
  levels,
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

  const stateLabel = {
    idle: t("overlay.hud.idle", "Ready"),
    starting: t("overlay.state.starting", "Starting"),
    listening: t("overlay.state.listening", "Listening"),
    transcribing: t("overlay.state.transcribing", "Transcribing"),
    processing: t("overlay.state.processing", "Processing"),
    error: t("overlay.state.failed", "Failed"),
  }[hud];

  const errorLabelKey = error && errorTitleKey(error.error_type);
  const failureText = (errorLabelKey && t(errorLabelKey)) || stateLabel;

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
  const listening = hud === "listening";
  const meterLevels = levels.length === 0 ? RESTING_METER_BARS : levels;

  const instrumentRow = (
    <div className="sbase">
      {failed ? (
        <span className="serror" role="alert">
          {failureText}
        </span>
      ) : listening ? (
        <>
          <span
            className="sr-only"
            role="status"
            aria-live="polite"
            aria-atomic="true"
          >
            {stateLabel}
          </span>
          <div
            className={`swave snap-measured${hearingSpeech(levels) ? " hearing" : ""}`}
            role="img"
            aria-label={t("overlay.inputLevel", "Input level")}
          >
            {Array.from({ length: METER_BAR_COUNT }, (_, index) => (
              <i
                key={index}
                style={{
                  transform:
                    "scaleY(" + barScale(meterLevels[index] ?? 0) + ")",
                }}
              />
            ))}
          </div>
        </>
      ) : (
        <span
          className="sstate"
          role="status"
          aria-live="polite"
          aria-atomic="true"
        >
          {stateLabel}
        </span>
      )}
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

  if (frame === "stream") {
    const hasText =
      streamText.committed.length > 0 || streamText.tentative.length > 0;

    return (
      <div dir={direction} className={`ov-stage ${position}`}>
        <div
          key={session}
          className={`scard hud-${hud}${hasText ? " open" : ""}`}
          data-testid="hud-card"
        >
          <div className="stext">
            <div className="stext-clip">
              <div className="stext-cap" ref={capRef} onScroll={onStreamScroll}>
                <p>
                  <span className="committed">
                    {streamText.committed ? streamText.committed + " " : ""}
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
