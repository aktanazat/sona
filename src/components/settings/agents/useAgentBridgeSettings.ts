import { useSettings } from "@/hooks/useSettings";
import { useAgentBridgeActions } from "./useAgentBridgeActions";
import { useAgentBridgeObservations } from "./useAgentBridgeObservations";

/* The whole page reads one model: the observed state, the values derived from
 * it, and the actions that write it. */
export const useAgentBridgeSettings = (active = true) => {
  const { refreshSettings, settings } = useSettings();
  const {
    view,
    updateView,
    replySessions,
    expiryTimeFormatter,
    refreshObservations,
  } = useAgentBridgeObservations(settings?.agent_bridge, active);
  const actions = useAgentBridgeActions({
    view,
    updateView,
    refreshObservations,
    refreshSettings,
  });

  const interactiveReady =
    view.bridge.master_enabled && view.status?.diagnostic === "active";

  return {
    ...view,
    replySessions,
    interactiveReady,
    expiryTimeFormatter,
    updateView,
    ...actions,
    refreshObservations,
  };
};

export type AgentBridgeSettingsModel = ReturnType<
  typeof useAgentBridgeSettings
>;
