import React from "react";
import { AgentBridgePendingReplies } from "./AgentBridgePendingReplies";
import { AgentBridgeReplyComposer } from "./AgentBridgeReplyComposer";
import type { AgentBridgeSettingsModel } from "./useAgentBridgeSettings";

export const AgentBridgeQueue: React.FC<{
  model: AgentBridgeSettingsModel;
}> = ({ model }) => (
  <>
    <AgentBridgeReplyComposer
      replySessionId={model.replySessionId}
      replyText={model.replyText}
      replySessions={model.replySessions}
      interactiveReady={model.interactiveReady}
      updateView={model.updateView}
      createReplyPreview={model.createReplyPreview}
    />
    <AgentBridgePendingReplies
      pendingMessages={model.pendingMessages}
      confirmPending={model.confirmPending}
      cancelPending={model.cancelPending}
    />
  </>
);
