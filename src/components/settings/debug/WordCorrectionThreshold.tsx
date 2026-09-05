import React from "react";
import { useTranslation } from "react-i18next";
import { RowReset, SettingsRow } from "@/components/settings/rows";
import { Slider } from "@/components/vg/slider";
import { useModelStore } from "@/stores/modelStore";
import { takesVocabularyAsPrompt } from "../models/modelFamily";
import { useSettings } from "../../../hooks/useSettings";

/* The backend's own default (settings.rs). Written once, because it is now
 * read twice: as the value an unread store shows, and as the comparison that
 * decides whether this row has anything to reset. */
const DEFAULT_THRESHOLD = 0.18;

export const WordCorrectionThreshold: React.FC = () => {
  const { t } = useTranslation();
  const { settings, updateSetting, resetSetting, isUpdating } = useSettings();
  const currentModel = useModelStore((state) => state.currentModel);
  const models = useModelStore((state) => state.models);
  const label = t("settings.debug.wordCorrectionThreshold.title");
  const value = settings?.word_correction_threshold ?? DEFAULT_THRESHOLD;
  const busy = isUpdating("word_correction_threshold");
  /* Whisper's decoder is handed the vocabulary as its prompt, so
   * `post_process_transcription_text` corrects only exact repeats of what it
   * already saw and never reads this threshold (managers/transcription.rs).
   * A row that answers the drag while changing nothing about the transcript
   * is the worse half of that: it says the correction is tunable here. The
   * reset goes with the slider rather than staying live on a dimmed row -
   * both controls write the same unread field, so either both belong to this
   * model or neither does. */
  const prompted = models.some(
    (model) => model.id === currentModel && takesVocabularyAsPrompt(model),
  );

  return (
    <SettingsRow
      label={label}
      disabled={prompted}
      fact={
        prompted
          ? t("settings.debug.wordCorrectionThreshold.notApplied")
          : value.toFixed(2)
      }
      hint={
        prompted
          ? t("settings.debug.wordCorrectionThreshold.promptedHint")
          : undefined
      }
    >
      <Slider
        aria-label={label}
        className="w-40"
        value={[value]}
        min={0}
        max={1}
        step={0.01}
        disabled={busy || prompted}
        onValueChange={([next]) =>
          void updateSetting("word_correction_threshold", next)
        }
      />
      <RowReset
        name={label}
        changed={value !== DEFAULT_THRESHOLD}
        disabled={busy || prompted}
        onReset={() => void resetSetting("word_correction_threshold")}
      />
    </SettingsRow>
  );
};
