import React from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import type { RecordingRetentionPeriod } from "@/bindings";
import { Input } from "@/components/vg/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import {
  SettingsDisclosure,
  SettingsLinkRow,
  SettingsRow,
  SettingsSection,
} from "@/components/settings/rows";
import { useSettings } from "@/hooks/useSettings";
import { useModelStore } from "@/stores/modelStore";
import { AccelerationSelector } from "../AccelerationSelector";
import { ChannelSelector } from "../ChannelSelector";
import { CommandMode } from "../CommandMode";
import { ExperimentalToggle } from "../ExperimentalToggle";
import { HudPillRow } from "../HudPillRow";
import { LazyStreamClose } from "../LazyStreamClose";
import { ModelUnloadTimeoutSetting } from "../ModelUnloadTimeout";
import { ShortcutInput } from "../ShortcutInput";
import { ShowOverlay } from "../ShowOverlay";
import { TranslateToEnglish } from "../TranslateToEnglish";
import { KeyboardImplementationSelector } from "../debug/KeyboardImplementationSelector";
import { PrivacyContextSettings } from "../privacy/PrivacyContextSettings";
import { useDataRetention } from "../useDataRetention";

const SPELLING_ID = "advanced-english-spelling";
const HISTORY_LIMIT_ID = "advanced-history-limit";
const RETENTION_ID = "advanced-recording-retention";

/* The periods the backend accepts, in the order a reader scans them: never,
 * then the count-based limit, then the three clocks. */
const RETENTION_OPTIONS = [
  "never",
  "preserve_limit",
  "days_3",
  "weeks_2",
  "months_3",
] as const satisfies readonly RecordingRetentionPeriod[];

/* What happens to a dictation between the microphone and the text.
 *
 * One section where Advanced used to have three — Launch, Processing and
 * Experimental. Launch is gone: autostart is the only row anyone touched and
 * it sits on Essentials now. Experimental survives as a switch that visibly
 * extends this section rather than revealing one somewhere else.
 *
 * Two things moved here in round 7, because both answer to this subject
 * rather than to a decision made on the day Sona is installed: how long
 * recordings survive, which is the pair of how many dictations are kept, and
 * the door to the dictation-style editor, which is the transformation this
 * whole section is about.
 *
 * What Sona may read from other apps is behind a disclosure because it is a
 * ceiling, not a preference: it is set once, deliberately, and the default of
 * "nothing" is the one most people keep. The level rides on the summary, so
 * the answer is readable without opening it. */
export const AdvancedDictation: React.FC<{ onOpenModes: () => void }> = ({
  onOpenModes,
}) => {
  const { t } = useTranslation();
  const { settings, updateSetting, getSetting } = useSettings();
  const {
    errorNotice,
    dataUpdating,
    historyLimit,
    updateHistoryLimit,
    retentionPeriod,
    updateRetentionPeriod,
  } = useDataRetention();
  const currentModel = useModelStore((state) => state.currentModel);
  const models = useModelStore((state) => state.models);

  const experimentalEnabled = settings?.experimental_enabled ?? false;
  const englishSpelling = settings?.english_spelling ?? "as_spoken";
  const commandModeEnabled = getSetting("command_mode_enabled") ?? true;
  const pushToTalk = getSetting("push_to_talk");
  const contextCeiling = getSetting("context_policy_ceiling") ?? "none";
  const supportsTranslation =
    models.find((model) => model.id === currentModel)?.supports_translation ??
    false;
  /* Linux has no separate cancel chord: the platform reports the release of a
   * held key differently, so push to talk is the only cancel path there. */
  const isLinux = type() === "linux";

  return (
    <SettingsSection label={t("settingsV2.advanced.dictation")}>
      {/* The cancel chord exists only when a tap-to-start binding can leave a
       * recording running with nothing holding it. */}
      {!isLinux && !pushToTalk ? <ShortcutInput shortcutId="cancel" /> : null}
      <CommandMode />
      {commandModeEnabled ? <ShortcutInput shortcutId="command" /> : null}

      {/* The microphone end of the same path. Present only on a device with
       * more than one channel, which is why it sits under the shortcuts rather
       * than beside the microphone picker on Essentials: on most machines
       * there is no row here at all. */}
      <ChannelSelector />

      {/* The two option names are the description: "As spoken" or "British"
       * says everything a sentence under the row would. */}
      <SettingsRow
        label={t("settings.advanced.englishSpelling.label")}
        controlId={SPELLING_ID}
      >
        <Select
          value={englishSpelling}
          onValueChange={(value) => {
            if (value !== "as_spoken" && value !== "british") return;
            void updateSetting("english_spelling", value);
          }}
        >
          <SelectTrigger id={SPELLING_ID} size="sm">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="as_spoken">
              {t("settings.advanced.englishSpelling.values.as_spoken")}
            </SelectItem>
            <SelectItem value="british">
              {t("settings.advanced.englishSpelling.values.british")}
            </SelectItem>
          </SelectContent>
        </Select>
      </SettingsRow>
      {supportsTranslation ? <TranslateToEnglish /> : null}
      {/* The editor for the styles a dictation is written through: not a
       * setting but a door, and this is the section its subject belongs to. */}
      <SettingsLinkRow
        label={t("settingsV2.essentials.dictationStyles")}
        action={t("settingsV2.essentials.dictationStylesAction")}
        onOpen={onOpenModes}
      />
      <ShowOverlay />
      {/* Beside the recording overlay, because both are what Sona puts on
       * screen outside its own window. */}
      <HudPillRow />
      <ModelUnloadTimeoutSetting />

      <SettingsRow
        label={t("settingsV2.advanced.keepDictations")}
        hint={t("settings.privacy.data.historyLimit.description")}
        controlId={HISTORY_LIMIT_ID}
      >
        <Input
          id={HISTORY_LIMIT_ID}
          type="number"
          min="0"
          max="1000"
          value={historyLimit}
          onChange={(event) => void updateHistoryLimit(event.target.value)}
          disabled={dataUpdating}
          className="w-20"
        />
      </SettingsRow>
      {/* The other half of the same question. One error line serves both
       * writes, which is why they are adjacent and not in two places. */}
      <SettingsRow
        label={t("settingsV2.essentials.retention")}
        controlId={RETENTION_ID}
      >
        <Select
          value={retentionPeriod}
          disabled={dataUpdating}
          onValueChange={(period) => {
            const next = RETENTION_OPTIONS.find(
              (candidate) => candidate === period,
            );
            if (next) void updateRetentionPeriod(next);
          }}
        >
          {/* No fixed width: the retention labels are long in several locales
           * and a Select trigger clips them with no ellipsis. */}
          <SelectTrigger id={RETENTION_ID} size="sm">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {RETENTION_OPTIONS.map((period) => (
              <SelectItem key={period} value={period}>
                {t("settings.privacy.data.retention.values." + period)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsRow>
      {errorNotice}

      <SettingsDisclosure
        label={t("settingsV2.advanced.readsFromOtherApps")}
        fact={t("settings.privacy.context.ceiling.values." + contextCeiling)}
      >
        <PrivacyContextSettings />
      </SettingsDisclosure>

      <ExperimentalToggle />
      {experimentalEnabled ? (
        <>
          <KeyboardImplementationSelector />
          <AccelerationSelector />
          <LazyStreamClose />
        </>
      ) : null}
    </SettingsSection>
  );
};
