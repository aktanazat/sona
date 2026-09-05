import type {
  StreamEngine,
  StreamPhase,
  StreamPhaseEvent,
  StreamTextEvent,
  StreamWorkKind,
} from "@/bindings";
import type { OverlayPosition } from "@/lib/powerPackApi";
import type { RecordingErrorEvent } from "@/lib/types/events";
import type { OverlayChrome, OverlayState } from "./overlayEvents";

/**
 * The HUD's five transient states plus its resting one. Every state the user can
 * observe has a name here, and `deriveHudPhase` is the only place they are
 * decided — the renderer switches on the result instead of re-deducing it from
 * four booleans.
 *
 * `processing` rather than "delivering": the only two work signals the backend
 * emits are `StreamWorkKind::Transcribing` and `::Polishing`, and the compact
 * `processing` overlay state is shown when a run has post-processing. Delivery
 * itself is never a state — `deliver` runs and `hide_recording_overlay` is
 * called in the same closure, so a "Delivering" word would mislabel LLM
 * post-processing as a paste.
 */
export type HudPhase =
  | "idle"
  | "starting"
  | "listening"
  | "transcribing"
  | "processing"
  | "error";

/**
 * Which window the HUD is being drawn into. The overlay window is resized by
 * `overlay_dimensions` in `src-tauri/src/overlay.rs` before each show, so the
 * frame — not the phase — is what decides whether there is room for two lines.
 *
 *   compact  256x46   stream  400x120   pill  184x36
 *
 * The `pill` frame only exists while the idle pill is enabled; it is off by
 * default, and the backend then unmaps the window instead of resting into it.
 */
export type HudFrame = "compact" | "stream" | "pill";

export interface HudState {
  isVisible: boolean;
  state: OverlayState;
  /** True once the input stream has delivered its first buffer. */
  captureReady: boolean;
  streamText: StreamTextEvent;
  phase: StreamPhase;
  workKind: StreamWorkKind;
  engine: StreamEngine;
  /** Wall clock at first captured buffer; null until the mic is actually live. */
  readyAt: number | null;
  /**
   * Wall clock the elapsed readout is computed against. Stamped once when the
   * capture ends, so the frozen number is the real capture length rather than
   * whatever the last timer tick happened to catch.
   */
  nowMs: number;
  session: number;
  position: OverlayPosition;
  modeName: string | null;
  /** Raw chord, e.g. "option_left+shift+space"; "" when the record is missing. */
  stopChord: string;
  error: RecordingErrorEvent | null;
  /** Where the HUD goes once a latched failure has been read. */
  restAfterError: "hide" | "idle" | null;
}

export const INITIAL_HUD_STATE: HudState = {
  isVisible: false,
  state: "recording",
  captureReady: false,
  streamText: { committed: "", tentative: "" },
  phase: "listening",
  workKind: "transcribing",
  engine: "local",
  readyAt: null,
  nowMs: 0,
  session: 0,
  position: "bottom",
  modeName: null,
  stopChord: "",
  error: null,
  restAfterError: null,
};

const EMPTY_STREAM_TEXT: StreamTextEvent = { committed: "", tentative: "" };

/**
 * Freeze the elapsed readout at the instant the capture stopped. A null
 * `readyAt` means nothing was ever measured, so there is nothing to freeze.
 */
const captureEndPatch = (state: HudState): { nowMs: number } | null =>
  state.readyAt === null ? null : { nowMs: Date.now() };

/**
 * `starting` and `listening` are the split that matters, and it is decided by
 * `captureReady` alone — never by `StreamPhase`. `StreamPhase::Listening` is
 * documented as "receiving audio *or waiting for the stream to begin*" and Rust
 * never emits it; the frontend starts in it. Reading the word off `phase` would
 * print "Listening" from the instant the card mounts, through the whole
 * 140-215 ms of `build_input_stream` (unbounded on Bluetooth) during which
 * nothing is captured at all. That is the head loss this state exists to stop.
 */
export const deriveHudPhase = (state: HudState): HudPhase => {
  if (state.error) return "error";
  if (state.state === "idle") return "idle";
  if (state.state === "transcribing") return "transcribing";
  if (state.state === "processing") return "processing";
  if (state.state === "streaming" && state.phase === "working") {
    return state.workKind === "polishing" ? "processing" : "transcribing";
  }
  return state.captureReady ? "listening" : "starting";
};

export const deriveHudFrame = (state: HudState): HudFrame =>
  state.state === "streaming"
    ? "stream"
    : state.state === "idle"
      ? "pill"
      : "compact";

/** Null while nothing has been measured, which is what `starting` renders. */
export const deriveElapsedSeconds = (state: HudState): number | null =>
  state.readyAt === null ? null : (state.nowMs - state.readyAt) / 1000;

/**
 * One second of wall clock, while the microphone is open.
 *
 * Only an open capture may advance: `captureEndPatch` freezes `nowMs` at the
 * instant the run stopped, and a tick landing after that would walk the frozen
 * capture length forward into a number no recorder ever measured. The overlay
 * only runs its interval while the phase is `listening`, but clearing an
 * interval races the state change that stopped the capture, so the refusal
 * lives here — in the machine that owns the readout — rather than in the one
 * caller that happens to schedule it.
 */
export const hudTicked = (state: HudState): HudState => {
  const capturing = state.state === "recording" || state.state === "streaming";
  if (state.readyAt === null || !capturing) return state;
  return { ...state, nowMs: Date.now() };
};

/**
 * `show-overlay`. A transient show resets everything about the previous run,
 * including readiness: the microphone is not open yet, and Rust queues
 * `recording-ready` onto the main thread *after* this event so the reset can
 * never overtake it.
 */
export const hudShown = (state: HudState, shown: OverlayState): HudState => {
  const transient = shown === "recording" || shown === "streaming";
  const resting = shown === "idle";
  return {
    ...state,
    isVisible: true,
    state: shown,
    // A new dictation retires a latched failure; resting does not, so the pill
    // can hold the failure until it has been read.
    error: resting ? state.error : null,
    restAfterError: resting && state.error ? "idle" : null,
    ...(transient
      ? {
          captureReady: false,
          readyAt: null,
          streamText: EMPTY_STREAM_TEXT,
        }
      : null),
    /* SAFETY: the three literals below are members of the unions they are
       asserted to, and this is the one place the machine re-arms a streaming
       session, so the frame it writes is the frame those unions describe. */
    ...(shown === "streaming"
      ? {
          phase: "listening" as StreamPhase,
          workKind: "transcribing" as StreamWorkKind,
          engine: "local" as StreamEngine,
          session: state.session + 1,
        }
      : null),
    ...(shown === "transcribing" || shown === "processing"
      ? captureEndPatch(state)
      : null),
  };
};

/**
 * `hide-overlay`. Every terminal failure path in `actions.rs` emits
 * `recording-error` and then calls `hide_recording_overlay` back to back, so the
 * two arrive in the same tick. A hide therefore records where the HUD should
 * rest instead of tearing a latched failure down unread.
 */
export const hudHidden = (state: HudState): HudState =>
  state.error
    ? { ...state, restAfterError: "hide" }
    : { ...state, isVisible: false, captureReady: false, readyAt: null };

/** `recording-ready` — the first captured buffer, and the clock's origin. */
export const hudCaptureReady = (state: HudState, atMs: number): HudState => ({
  ...state,
  captureReady: true,
  readyAt: atMs,
  nowMs: atMs,
});

export const hudStreamPhaseChanged = (
  state: HudState,
  event: StreamPhaseEvent,
): HudState => ({
  ...state,
  ...(event.phase === "working" ? captureEndPatch(state) : null),
  phase: event.phase,
  workKind: event.kind ?? state.workKind,
});

/** `recording-error`. The HUD names the failure on the surface the user was
 * already watching; the main window's toast stays the long-form explanation. */
export const hudFailed = (
  state: HudState,
  error: RecordingErrorEvent,
): HudState => ({
  ...state,
  ...captureEndPatch(state),
  isVisible: true,
  error,
  restAfterError: null,
});

/** The failure has been on screen for its dwell; take the rest that was asked
 * for while it was being read. */
export const hudRested = (state: HudState): HudState => ({
  ...state,
  error: null,
  restAfterError: null,
  isVisible: state.restAfterError !== "hide",
  state: state.restAfterError === "idle" ? "idle" : state.state,
  captureReady: false,
  readyAt: null,
});

export const hudChromeRead = (
  state: HudState,
  chrome: OverlayChrome,
): HudState => ({
  ...state,
  position: chrome.position,
  stopChord: chrome.stopChord,
});
