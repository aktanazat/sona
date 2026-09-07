import React from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import { Microlabel, SettingsPage } from "@/components/settings/rows";
import type { SettingsNavigationRequest } from "../navigation";
import { AboutSections } from "../about/AboutSections";
import { AdvancedAgents } from "./AdvancedAgents";
import { AdvancedDictation } from "./AdvancedDictation";
import { AdvancedMeetings } from "./AdvancedMeetings";
import { AdvancedModels } from "./AdvancedModels";
import { AdvancedSync } from "./AdvancedSync";
import { AdvancedWorkflows } from "./AdvancedWorkflows";

export const AdvancedSettings: React.FC<{
  navigationRequest?: SettingsNavigationRequest | null;
  onOpenCatalog: () => void;
  onOpenModes: () => void;
  onOpenPrompts: () => void;
}> = ({ navigationRequest, onOpenCatalog, onOpenModes, onOpenPrompts }) => {
  const { t } = useTranslation();
  const target =
    navigationRequest?.target.tab === "advanced"
      ? navigationRequest.target.section
      : undefined;

  return (
    <SettingsPage title={t("settingsV2.advanced.title")}>
      <AdvancedMeetings
        onOpenPrompts={onOpenPrompts}
        revealRequest={
          target === "meetings" ? navigationRequest?.nonce : undefined
        }
      />
      <AdvancedModels onOpenCatalog={onOpenCatalog} />
      <AdvancedDictation onOpenModes={onOpenModes} />
      <AdvancedWorkflows />
      <AdvancedSync />
      <AdvancedAgents
        revealSonaAgentRequest={
          target === "sonaAgent" ? navigationRequest?.nonce : undefined
        }
      />
      <AboutSections />
      <Microlabel>
        {t("settingsV2.advanced.debugHint", {
          chord: type() === "macos" ? "\u2318\u21e7D" : "Ctrl+Shift+D",
        })}
      </Microlabel>
    </SettingsPage>
  );
};
