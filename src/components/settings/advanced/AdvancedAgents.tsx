import React from "react";
import { useTranslation } from "react-i18next";
import {
  SettingsDisclosure,
  SettingsSection,
} from "@/components/settings/rows";
import { useSettings } from "@/hooks/useSettings";
import { BooleanSettingRow } from "../BooleanSettingRow";
import { AgentPanelToggle } from "../agents/AgentPanelToggle";
import { SonaAgentPairing } from "../agents/SonaAgentPairing";
import { AgentBridgeWorkspace } from "../agents/AgentBridgeWorkspace";

/* Which consoles the bridge is currently observing. The switch names live
 * beside the switches inside, so the summary reuses them rather than
 * inventing a second vocabulary for the same four programs. */
const BRIDGE_CONSOLES = [
  ["claude_enabled", "settings.agents.controls.providers.claude.label"],
  ["codex_enabled", "settings.agents.controls.providers.codex.label"],
  ["grok_enabled", "settings.agents.controls.providers.grok.label"],
  ["omp_enabled", "settings.agents.controls.providers.omp.label"],
] as const;

/* The four switches that decide whether Sona talks to anything else on this
 * Mac, and the consoles behind them.
 *
 * The panel is the window agents speak through; the bridge is what a local
 * coding agent may ask Sona to do; external access is what any other program
 * may read, and external mutations is the one thing they may change. All four
 * are off-by-default policy, so they are rows. Everything each one then shows —
 * the relay it is paired with, the sessions, the permission requests, the reply
 * queue, the rules — is a console you open when something is happening, so each
 * is one disclosure rather than a dozen sections that are empty on a Mac with
 * nothing connected.
 *
 * The two external rows have no console: each grants one headless surface and
 * there is nothing to watch, so the hints carry the whole consequence. They are
 * two rows rather than one because letting a script read the corpus and letting
 * it close a loop are different answers, and a single switch would answer both
 * at once.
 *
 * Each console states its own answer on the summary - paired or not, which
 * agents are observed - so a reader learns whether anything is connected
 * without opening either one. */
export const AdvancedAgents: React.FC = () => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const bridge = settings?.agent_bridge;
  const observed = bridge?.master_enabled
    ? BRIDGE_CONSOLES.filter(([key]) => bridge[key]).map(([, label]) =>
        t(label),
      )
    : [];

  return (
    <SettingsSection label={t("settingsV2.advanced.agents")}>
      <AgentPanelToggle />
      <BooleanSettingRow
        settingKey="external_query_enabled"
        labelKey="settings.agents.externalAccess.label"
        hintKey="settings.agents.externalAccess.rowDescription"
      />
      <BooleanSettingRow
        settingKey="external_mutations_enabled"
        labelKey="settings.agents.externalMutations.label"
        hintKey="settings.agents.externalMutations.rowDescription"
      />
      <SettingsDisclosure
        label={t("settings.agents.sonaAgent.title")}
        fact={t(
          settings?.agent_panel_paired
            ? "settings.agents.sonaAgent.paired"
            : "settings.agents.sonaAgent.unpaired",
        )}
        lazy
      >
        <SonaAgentPairing />
      </SettingsDisclosure>
      <SettingsDisclosure
        label={t("settingsV2.advanced.agentBridge")}
        fact={
          observed.length === 0
            ? t("settings.agents.status.disabled")
            : observed.join(", ")
        }
        lazy
      >
        <AgentBridgeWorkspace />
      </SettingsDisclosure>
    </SettingsSection>
  );
};
