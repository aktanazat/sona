import React, { useEffect, useLayoutEffect, useReducer, useRef } from "react";
import { useTranslation } from "react-i18next";
import { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection, initializeRTL } from "@/lib/utils/rtl";
import { getHudPillState } from "@/lib/powerPackApi";
import { readOverlayChrome, subscribeToOverlayEvents } from "./overlayEvents";
import {
  deriveElapsedSeconds,
  deriveHudFrame,
  deriveHudPhase,
  hudCaptureReady,
  hudChromeRead,
  hudFailed,
  hudHidden,
  hudRested,
  hudShown,
  hudStreamPhaseChanged,
  hudTicked,
  INITIAL_HUD_STATE,
  type HudState,
} from "./hudMachine";
import { RecordingOverlayContent } from "./RecordingOverlayContent";

/**
 * How long a failure holds the HUD before it rests.
 *
 * Only reachable in full when the idle pill is enabled: resting into the pill
 * bumps `OVERLAY_SHOW_GENERATION` and cancels the pending unmap, so the window
 * stays on screen. With the pill off (the default) `hide_overlay_window` unmaps
 * the window 300 ms after `hide-overlay` regardless of what the webview is
 * painting, and that 300 ms is the hard ceiling — extending it would need a
 * deferred hide in `overlay.rs`.
 */
const ERROR_DWELL_MS = 4000;

type HudAction = (state: HudState) => HudState;

const hudReducer = (state: HudState, action: HudAction) => action(state);

const RecordingOverlay: React.FC = () => {
  const { i18n } = useTranslation();
  const [hud, dispatch] = useReducer(hudReducer, INITIAL_HUD_STATE);
  const disposedRef = useRef(false);
  // Live-text scroll-back: the text region "sticks" to the newest line while the
  // user is at the bottom; if they scroll up to read history, auto-follow pauses
  // until they scroll back down.
  const capRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);
  const direction = getLanguageDirection(i18n.language);

  /* The overlay is its own document and owns its own `lang`/`dir`. `index.html`
   * ships `lang="en"` and nothing updated it, so a Turkish or Hebrew HUD was
   * shaped, hyphenated and mirrored as English: `dir` drives which edge the
   * mark sits on and which way the error line reads, and both are wrong at
   * `lang="en"`. The main window already does this via `initializeRTL`; a
   * second webview cannot inherit it, for the same reason it cannot inherit
   * the theme. */
  useEffect(() => {
    initializeRTL(i18n.language);
  }, [i18n.language]);

  useEffect(() => {
    disposedRef.current = false;
    const subscriptions = subscribeToOverlayEvents({
      onShow: async (overlayState) => {
        if (disposedRef.current) return;
        // Paint before the settings reads, not after: the shortcut has already
        // fired and every millisecond spent here is a millisecond in which the
        // user has no acknowledgement at all.
        dispatch((current) => hudShown(current, overlayState));

        await syncLanguageFromSettings();
        if (disposedRef.current) return;

        try {
          const chrome = await readOverlayChrome();
          if (disposedRef.current) return;
          dispatch((current) => hudChromeRead(current, chrome));
        } catch {
          // Keep the previous placement and stop hint if settings can't be read.
        }

        try {
          const pill = await getHudPillState();
          if (disposedRef.current) return;
          dispatch((current) => ({ ...current, modeName: pill.mode_name }));
        } catch {
          // The mode name is omitted rather than guessed.
        }
      },
      onHide: () => {
        if (!disposedRef.current) dispatch(hudHidden);
      },
      onRecordingReady: () => {
        if (disposedRef.current) return;
        const readyAtMs = Date.now();
        dispatch((current) => hudCaptureReady(current, readyAtMs));
      },
      onStreamText: (streamText) => {
        if (!disposedRef.current)
          dispatch((current) => ({ ...current, streamText }));
      },
      onStreamPhase: (event) => {
        if (disposedRef.current) return;
        dispatch((current) => hudStreamPhaseChanged(current, event));
      },
      onStreamEngine: (event) => {
        if (disposedRef.current) return;
        dispatch((current) => ({ ...current, engine: event.engine }));
      },
      onRecordingError: (error) => {
        if (disposedRef.current) return;
        dispatch((current) => hudFailed(current, error));
      },
    });

    return () => {
      disposedRef.current = true;
      for (const subscription of subscriptions) {
        void subscription.then(
          (unlisten) => unlisten(),
          (error) => console.error("Overlay event subscription failed:", error),
        );
      }
    };
  }, []);

  // A latched failure holds the HUD for one dwell, then releases it to whatever
  // rest the backend asked for while it was being read.
  useEffect(() => {
    if (!hud.error || !hud.restAfterError) return;
    const id = setTimeout(() => dispatch(hudRested), ERROR_DWELL_MS);
    return () => clearTimeout(id);
  }, [hud.error, hud.restAfterError]);

  /* One tick a second, and only while the microphone is open. `listening` is
   * the one phase with a running clock: `starting` has measured nothing yet,
   * and the working phases read the capture length `captureEndPatch` froze at
   * the real end of the run. A stopped interval is also the whole reason this
   * window is idle between runs — the meter it replaced dispatched on every
   * `mic-level` event, which is a re-render per audio frame. */
  const phase = deriveHudPhase(hud);
  useEffect(() => {
    if (phase !== "listening") return;
    const id = setInterval(() => dispatch(hudTicked), 1000);
    return () => clearInterval(id);
  }, [phase]);

  // Stick to the bottom as text streams in — but only while pinned, so a user
  // who has scrolled up to read history isn't yanked back down by the next chunk.
  useLayoutEffect(() => {
    const el = capRef.current;
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight;
  }, [hud.streamText]);

  // Each fresh streaming session starts pinned to the bottom.
  useEffect(() => {
    pinnedRef.current = true;
  }, [hud.session]);

  // Re-pin when the user is within ~a line of the bottom; unpin otherwise.
  const handleStreamScroll = () => {
    const element = capRef.current;
    if (!element) return;
    pinnedRef.current =
      element.scrollHeight - element.scrollTop - element.clientHeight <= 16;
  };

  return (
    <RecordingOverlayContent
      isVisible={hud.isVisible}
      hud={phase}
      frame={deriveHudFrame(hud)}
      elapsedSeconds={deriveElapsedSeconds(hud)}
      streamText={hud.streamText}
      modeName={hud.modeName}
      error={hud.error}
      session={hud.session}
      position={hud.position}
      direction={direction}
      capRef={capRef}
      onStreamScroll={handleStreamScroll}
    />
  );
};

export default RecordingOverlay;
