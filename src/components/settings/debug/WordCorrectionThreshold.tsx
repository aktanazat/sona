import React from "react";
import { useTranslation } from "react-i18next";
import { RowReset, SettingsRow } from "@/components/settings/rows";
import { Slider } from "@/components/vg/slider";
import { useSettings } from "../../../hooks/useSettings";

/* The backend's own default (settings.rs). Written once, because it is now
 * read twice: as the value an unread store shows, and as the comparison that
 * decides whether this row has anything to reset. */
const DEFAULT_THRESHOLD = 0.18;

export const WordCorrectionThreshold: React.FC = () => {
  const { t } = useTranslation();
  const { settings, updateSetting, resetSetting, isUpdating } = useSettings();
  const label = t("settings.debug.wordCorrectionThreshold.title");
  const value = settings?.word_correction_threshold ?? DEFAULT_THRESHOLD;
  const busy = isUpdating("word_correction_threshold");

  return (
    <SettingsRow label={label} fact={value.toFixed(2)}>
      <Slider
        aria-label={label}
        className="w-40"
        value={[value]}
        min={0}
        max={1}
        step={0.01}
        disabled={busy}
        onValueChange={([next]) =>
          void updateSetting("word_correction_threshold", next)
        }
      />
      <RowReset
        name={label}
        changed={value !== DEFAULT_THRESHOLD}
        disabled={busy}
        onReset={() => void resetSetting("word_correction_threshold")}
      />
    </SettingsRow>
  );
};
