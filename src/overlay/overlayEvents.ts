import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { commands, events } from "@/bindings";
import type {
  AppearanceMaterial,
  StreamEngineEvent,
  StreamPhaseEvent,
  StreamTextEvent,
} from "@/bindings";
import type { RecordingErrorEvent } from "@/lib/types/events";
import type { OverlayPosition } from "@/lib/powerPackApi";

/**
 * `idle` is the always-visible HUD pill: unlike the four transient states it is
 * not driven by a dictation, and the backend returns the overlay to it instead
 * of hiding when the pill is enabled.
 */
export type OverlayState =
  | "idle"
  | "recording"
  | "streaming"
  | "transcribing"
  | "processing";

/** Mirrors the `settings-changed` payload emitted across `shortcut/mod.rs`. */
interface SettingsChangedEvent {
  setting?: string | null;
  value?: unknown;
}

/**
 * Everything the overlay needs from settings, normalized so no consumer has to
 * re-decide a default. Read once per show, because the overlay is a separate
 * webview and cannot see the main window's own settings reads.
 */
export interface OverlayChrome {
  position: OverlayPosition;
  material: AppearanceMaterial;
  /**
   * The active-mode dictation chord, raw ("option_left+shift+space"), which is
   * also the chord that stops a run in progress. Empty when the binding record
   * is missing: the HUD then renders no stop hint rather than an empty keycap.
   */
  stopChord: string;
}

/** The chord id that starts and stops an active-mode dictation. */
const TRANSCRIBE_BINDING_ID = "transcribe";

export const readOverlayChrome = async (): Promise<OverlayChrome> => {
  const result = await commands.getAppSettings();
  if (result.status !== "ok") throw new Error(result.error);
  const settings = result.data;
  return {
    position: settings.overlay_position === "top" ? "top" : "bottom",
    material: settings.appearance_material === "glass" ? "glass" : "solid",
    stopChord:
      settings.bindings?.[TRANSCRIBE_BINDING_ID]?.current_binding ?? "",
  };
};

/**
 * Keep this document's `data-material` on the persisted appearance setting.
 *
 * The overlay is a second webview: it cannot see the main window's root
 * attribute, which is the same reason `theme-changed` exists for the theme.
 * Solid is written before the read resolves so the first paint is never glass
 * by accident, and the emitted value is the user's intent — Rust overwrites
 * this attribute directly on a window whose native material failed to apply.
 */
export const followAppearanceMaterial = async (): Promise<UnlistenFn> => {
  document.documentElement.dataset.material = "solid";
  const unlisten = await listen<SettingsChangedEvent>(
    "settings-changed",
    (event) => {
      if (event.payload?.setting !== "appearance_material") return;
      document.documentElement.dataset.material =
        event.payload.value === "glass" ? "glass" : "solid";
    },
  );
  try {
    const { material } = await readOverlayChrome();
    document.documentElement.dataset.material = material;
  } catch {
    // Solid is already applied; an unreadable setting keeps it.
  }
  return unlisten;
};

interface OverlayEventHandlers {
  onShow: (state: OverlayState) => void | Promise<void>;
  onHide: () => void;
  onRecordingReady: () => void;
  onStreamText: (text: StreamTextEvent) => void;
  onStreamPhase: (phase: StreamPhaseEvent) => void;
  onStreamEngine: (engine: StreamEngineEvent) => void;
  onRecordingError: (error: RecordingErrorEvent) => void;
}

/**
 * Translate Tauri events into the overlay's explicit handlers. This module owns
 * the overlay's backend seam — event registration and the one settings read;
 * RecordingOverlay owns state transitions and the caller owns the returned
 * cleanup functions.
 */
export const subscribeToOverlayEvents = ({
  onShow,
  onHide,
  onRecordingReady,
  onStreamText,
  onStreamPhase,
  onStreamEngine,
  onRecordingError,
}: OverlayEventHandlers): Array<Promise<UnlistenFn>> => [
  listen<OverlayState>("show-overlay", (event) => onShow(event.payload)),
  listen("hide-overlay", onHide),
  listen("recording-ready", onRecordingReady),
  events.streamTextEvent.listen((event) => onStreamText(event.payload)),
  events.streamPhaseEvent.listen((event) => onStreamPhase(event.payload)),
  events.streamEngineEvent.listen((event) => onStreamEngine(event.payload)),
  // `recording-error` is broadcast to every webview, so the HUD names the
  // failure on the surface the user was already watching. The main window's
  // toast stays the long-form explanation.
  listen<RecordingErrorEvent>("recording-error", (event) =>
    onRecordingError(event.payload),
  ),
];
