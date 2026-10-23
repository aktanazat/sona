import React, { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import type { HudPillEdge } from "@/bindings";
import { Switch } from "@/components/vg/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { SettingsRow } from "./rows";
import { useSettings } from "@/hooks/useSettings";
import {
  setHudPillEnabled,
  setHudPillPosition,
} from "@/lib/powerPackApi";

/* The always-visible idle pill, off by default.
 *
 * One row, two controls: where the pill sits is not a second setting, it is the
 * rest of this one, and it only exists once the pill does. The row this
 * replaced printed the position as a second full row, which is why the pill
 * cost two lines of a page for one decision.
 *
 * Both writes go through their own commands rather than the generic setting
 * updater because enabling the pill has to bring the overlay window on screen
 * immediately, not at the next dictation.
 */
export const HudPillRow: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const [enabled, setEnabled] = useState(false);
  const [position, setPosition] = useState<HudPillEdge>("bottom");
  const [pending, setPending] = useState(false);
  const enabledId = useId();
  const positionId = useId();

  useEffect(() => {
    if (!settings) return;
    setEnabled(settings.hud_pill_enabled ?? false);
    setPosition(settings.hud_pill_position ?? "bottom");
  }, [settings]);

  const write = async (apply: () => Promise<void>, revert: () => void) => {
    setPending(true);
    try {
      await apply();
      // Other surfaces read settings from the store, so leave its copy correct.
      await refreshSettings();
    } catch {
      revert();
    } finally {
      setPending(false);
    }
  };

  const enabledLabel = t("settings.hud.enabledLabel");
  const positionLabel = t("settings.hud.positionLabel");

  return (
    <SettingsRow
      label={enabledLabel}
      /* Kept: click-to-start and right-click-to-switch-modes are the pill's
       * whole interface, and nothing on screen shows them. */
      hint={t("settings.hud.enabledDescription")}
      hintLabel={enabledLabel}
      controlId={enabledId}
    >
      {enabled ? (
        <Select
          value={position}
          disabled={pending}
          onValueChange={(value) => {
            if (value !== "top" && value !== "bottom") return;
            const previous = position;
            setPosition(value);
            void write(
              () => setHudPillPosition(value),
              () => setPosition(previous),
            );
          }}
        >
          {/* The row's `<label for>` names the switch, so the select carries
           * its own accessible name rather than borrowing that one. */}
          <SelectTrigger
            id={positionId}
            size="sm"
            className="w-32"
            aria-label={positionLabel}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="top">{t("settings.hud.positionTop")}</SelectItem>
            <SelectItem value="bottom">
              {t("settings.hud.positionBottom")}
            </SelectItem>
          </SelectContent>
        </Select>
      ) : null}
      <Switch
        id={enabledId}
        checked={enabled}
        disabled={pending}
        onCheckedChange={(next) => {
          const previous = enabled;
          setEnabled(next);
          void write(
            () => setHudPillEnabled(next),
            () => setEnabled(previous),
          );
        }}
      />
    </SettingsRow>
  );
};
