import React from "react";
import { useTranslation } from "react-i18next";
import {
  SettingsDisclosure,
  SettingsSection,
} from "@/components/settings/rows";
import { CloudSyncPanel } from "../../cloud-sync/CloudSyncPanel";
import { PrivacyEgress } from "../privacy/PrivacyEgress";
import { PrivacyHistoryStorage } from "../privacy/PrivacyHistoryStorage";
import { PrivacyUpstreamImport } from "../privacy/PrivacyUpstreamImport";

/* Everything that can move data off this Mac, and the one reading that says
 * whether anything does.
 *
 * The Privacy tab is gone. Its controls went where the thing they govern is —
 * retention and context capture both sit under Dictation — and what is left is
 * what this section is for: read-only facts about egress, plus the one-time
 * setups that create it. The facts are collapsed because they are reassurance,
 * not a decision: an operator who wants them can open one row, and everyone
 * else reads a page that is not making a case. */
export const AdvancedSync: React.FC = () => {
  const { t } = useTranslation();

  return (
    <>
      <SettingsSection label={t("settingsV2.advanced.sync")}>
        <SettingsDisclosure label={t("settingsV2.advanced.leavesThisMac")}>
          <PrivacyEgress />
          <PrivacyHistoryStorage />
        </SettingsDisclosure>
      </SettingsSection>
      {/* Setup, recovery and pairing stay collapsed: they are a one-time
       * task, and they must not read as a switch that is simply off. */}
      <CloudSyncPanel />
      <PrivacyUpstreamImport />
    </>
  );
};
