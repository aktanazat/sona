import { ChevronUp } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LanguageDirection } from "@/lib/utils/rtl";
import {
  hudOpenModeMenu,
  hudToggleRecording,
  type OverlayPosition,
} from "@/lib/powerPackApi";

interface HudPillProps {
  position: OverlayPosition;
  direction: LanguageDirection;
  /** Null while the backend has not answered; the pill then reads "Ready". */
  modeName: string | null;
}

/**
 * The always-visible idle pill.
 *
 * It shares the recording overlay's window, so it is a state of that overlay
 * rather than a second window manager. The mode name is the resting content —
 * the only thing this pill knows that the user cannot see from the app — and
 * hover raises the two things you can do to it: record, and switch mode.
 *
 * Both actions are real buttons because the overlay is a non-activating
 * NSPanel: it cannot host a focusable webview popup, so the mode list is an OS
 * menu built on the Rust side. Right-clicking anywhere on the pill opens the
 * same menu, which is how the pill shipped and stays the discoverable path for
 * anyone who never hovers the trailing chevron.
 *
 * This is the one overlay surface that may take the Glass material: nothing
 * measured renders here, so a tint cannot contest a reading.
 */
export const HudPill = ({ position, direction, modeName }: HudPillProps) => {
  const { t } = useTranslation();
  const label = modeName ?? t("overlay.hud.idle", "Ready");

  return (
    <div dir={direction} className={`ov-stage ${position} ov-fade show`}>
      <div
        className="scard compact hud-pill"
        onContextMenu={(event) => {
          event.preventDefault();
          void hudOpenModeMenu();
        }}
        data-testid="hud-pill"
      >
        <button
          type="button"
          className="hud-pill-record"
          onClick={() => void hudToggleRecording()}
          aria-label={t("overlay.hud.toggle", {
            defaultValue: "Start dictation in {{mode}}",
            mode: label,
          })}
          title={t(
            "overlay.hud.hint",
            "Click to dictate, right-click for modes",
          )}
        >
          <span className="hud-pill-mode">{label}</span>
        </button>
        <button
          type="button"
          className="hud-pill-switch"
          onClick={() => void hudOpenModeMenu()}
          aria-label={t("overlay.hud.modes", "Switch mode")}
          title={t("overlay.hud.modes", "Switch mode")}
        >
          <ChevronUp aria-hidden="true" width={12} height={12} />
        </button>
      </div>
    </div>
  );
};
