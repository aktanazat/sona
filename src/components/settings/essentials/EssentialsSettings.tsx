import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsPage, SettingsSurface } from "@/components/settings/rows";
import { useModelStore } from "@/stores/modelStore";
import { AutostartToggle } from "../AutostartToggle";
import { LanguageSelector } from "../LanguageSelector";
import { MicrophoneSelector } from "../MicrophoneSelector";
import { PushToTalk } from "../PushToTalk";
import { ShortcutInput } from "../ShortcutInput";
import { MeetingAppsPicker } from "../meetings/MeetingAppsPicker";
import { MeetingDetectionToggle } from "../meetings/MeetingDetectionSettings";
import { SoundsRow } from "./SoundsRow";

/* The seven decisions a person makes once.
 *
 * No section labels: the tab above already names the page, and seven rows
 * under one hairline surface is the whole point — headings here would divide a
 * list short enough to read at once. Anything that needs a heading is not
 * essential and lives on Advanced.
 *
 * Four rows left in this round, and each one for the same reason: it is not a
 * decision this page is for. How long a recording survives and how many
 * dictations are kept are one subject, so they are one pair on Advanced >
 * Dictation. Appearance sits with the language Sona speaks and the material
 * its windows are made of, which are the other two once-in-an-install
 * choices. Dictation styles was never a setting at all — it is a door to an
 * editor, and the section that owns what happens to a dictation is where a
 * reader goes looking for it.
 *
 * Meeting apps stayed, as a disclosure: the summary names the apps that are
 * ticked, which is the only thing anyone reads it for. */
export const EssentialsSettings: React.FC = () => {
  const { t } = useTranslation();
  const { currentModel, models } = useModelStore();
  const model = models.find((candidate) => candidate.id === currentModel);

  return (
    <SettingsPage title={t("settingsV2.essentials.title")}>
      <SettingsSurface data-testid="settings-essentials">
        <ShortcutInput shortcutId="transcribe" />
        <PushToTalk />
        <MicrophoneSelector />
        {/* The spoken language, beside the microphone that hears it. The list
         * narrows to what the loaded model can recognise, so a model with one
         * language shows one language rather than a hundred it would ignore. */}
        <LanguageSelector
          supportedLanguages={model?.supported_languages}
          supportsLanguageDetection={model?.supports_language_detection}
        />
        <SoundsRow />
        <AutostartToggle />
        <MeetingDetectionToggle />
        <MeetingAppsPicker />
      </SettingsSurface>
    </SettingsPage>
  );
};
