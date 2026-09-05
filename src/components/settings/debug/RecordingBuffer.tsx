import React from "react";
import { useTranslation } from "react-i18next";
import { RowReset, SettingsRow } from "@/components/settings/rows";
import { Slider } from "@/components/vg/slider";
import { useSettings } from "../../../hooks/useSettings";

export const RecordingBuffer: React.FC = () => {
  const { t } = useTranslation();
  const { settings, updateSetting, resetSetting, isUpdating } = useSettings();
  const label = t("settings.debug.recordingBuffer.title");
  const value = settings?.extra_recording_buffer_ms ?? 0;
  const busy = isUpdating("extra_recording_buffer_ms");

  return (
    <SettingsRow
      label={label}
      hint={t("settings.debug.recordingBuffer.description")}
      fact={`${value}ms`}
    >
      <Slider
        aria-label={label}
        className="w-40"
        value={[value]}
        min={0}
        max={1500}
        step={50}
        disabled={busy}
        onValueChange={([next]) =>
          void updateSetting("extra_recording_buffer_ms", next)
        }
      />
      <RowReset
        name={label}
        changed={value !== 0}
        disabled={busy}
        onReset={() => void resetSetting("extra_recording_buffer_ms")}
      />
    </SettingsRow>
  );
};
