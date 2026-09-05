import React from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import { Microlabel, SettingsPage } from "@/components/settings/rows";
import { AboutSections } from "../about/AboutSections";
import { AdvancedAgents } from "./AdvancedAgents";
import { AdvancedDictation } from "./AdvancedDictation";
import { AdvancedMeetings } from "./AdvancedMeetings";
import { AdvancedModels } from "./AdvancedModels";
import { AdvancedSync } from "./AdvancedSync";
import { AdvancedWorkflows } from "./AdvancedWorkflows";

/* Everything that is not essential, in the order a person goes looking for it.
 *
 * Five tabs collapsed into this page: General's leftovers, Privacy, Agents,
 * Workflows and About. The order is by subject, not by how the code is
 * organised — meetings, then what recognises speech, then what happens to a
 * dictation, then what Sona does on its own, then what leaves this Mac, then
 * what talks to Sona, then what build this is. */
export const AdvancedSettings: React.FC<{
  onOpenCatalog: () => void;
  onOpenModes: () => void;
}> = ({ onOpenCatalog, onOpenModes }) => {
  const { t } = useTranslation();

  return (
    <SettingsPage title={t("settingsV2.advanced.title")}>
      <AdvancedMeetings />
      <AdvancedModels onOpenCatalog={onOpenCatalog} />
      <AdvancedDictation onOpenModes={onOpenModes} />
      <AdvancedWorkflows />
      <AdvancedSync />
      <AdvancedAgents />
      <AboutSections />
      {/* The debug page has no row and no link: it is a chord, and this is the
       * one line in the app that says so. */}
      <Microlabel>
        {t("settingsV2.advanced.debugHint", {
          chord: type() === "macos" ? "\u2318\u21e7D" : "Ctrl+Shift+D",
        })}
      </Microlabel>
    </SettingsPage>
  );
};
