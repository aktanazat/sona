import React, { useState } from "react";
import { RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/vg/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/vg/tabs";
import { Notice } from "@/components/settings/rows";
import { useAgentBridgeSettings } from "./useAgentBridgeSettings";
import { AgentBridgeControls } from "./AgentBridgeControls";
import { AgentBridgeHook } from "./AgentBridgeHook";
import { AgentBridgeQueue } from "./AgentBridgeQueue";
import { AgentBridgeProjects } from "./AgentBridgeProjects";
import { AgentBridgeRequests } from "./AgentBridgeRequests";
import { AgentBridgeRules } from "./AgentBridgeRules";
import { AgentBridgeSessions } from "./AgentBridgeSessions";

/* The coding-agent bridge's operator console: what it is allowed to do, what
 * it has been doing, and the replies waiting to be sent.
 *
 * It was the Agents tab, which is why it opened with a page title repeating
 * the tab above it. Settings has two tabs now, so this is a block Advanced
 * drops behind one disclosure row — the switches that decide whether any of it
 * runs are outside it, and this is what you open once they are on.
 *
 * One refresh for the whole console: sessions, requests, runtime status and
 * the pending queue all come from the same read. */
export const AgentBridgeWorkspace: React.FC = () => {
  const { t } = useTranslation();
  const model = useAgentBridgeSettings();
  const { refreshObservations, loading, error } = model;
  const [workspace, setWorkspace] = useState<"status" | "queue" | "rules">(
    "status",
  );
  const tabs = [
    { id: "status", label: t("settings.agents.observed.title") },
    { id: "queue", label: t("settings.agents.replyQueue.title") },
    { id: "rules", label: t("settings.agents.rules.title") },
  ] as const;

  return (
    <div className="flex flex-col gap-6 px-6 py-4">
      <div className="flex items-center justify-between gap-4">
        {error ? <Notice tone="danger">{error}</Notice> : <span />}
        <Button
          variant="outline"
          size="sm"
          onClick={() => void refreshObservations()}
          disabled={loading}
        >
          <RefreshCw
            aria-hidden="true"
            className={loading ? "animate-spin" : undefined}
          />
          {t("settings.agents.observed.refresh")}
        </Button>
      </div>
      <Tabs
        value={workspace}
        onValueChange={(id) => {
          const next = tabs.find((tab) => tab.id === id);
          if (next) setWorkspace(next.id);
        }}
        className="gap-0"
      >
        <div className="border-b border-gray-alpha-400">
          <TabsList
            variant="line"
            aria-label={t("settings.agents.workspaceNavigation", "Agent views")}
            className="w-full justify-start gap-6 px-0"
          >
            {tabs.map((tab) => (
              /* No className: the text-tab recipe this used to restate is the
                 kit's default now (vg/tabs.tsx). Its copy had drifted anyway —
                 `text-sm` is 12.25px at this app's 14px root, and the
                 `focus-visible:ring-blue-700` it carried was the last blue
                 focus ring in the app, where every other control draws the
                 bronze outline base.css paints. */
              <TabsTrigger key={tab.id} value={tab.id}>
                {tab.label}
              </TabsTrigger>
            ))}
          </TabsList>
        </div>
        <TabsContent value="status" className="flex flex-col gap-10 pt-8">
          <AgentBridgeControls
            bridge={model.bridge}
            status={model.status}
            mutateBridge={model.mutateBridge}
          />
          <AgentBridgeProjects
            bridge={model.bridge}
            authorizing={model.authorizing}
            authorizeProject={model.authorizeProject}
            mutateBridge={model.mutateBridge}
          />
          <AgentBridgeHook
            hookSnippet={model.hookSnippet}
            hookError={model.hookError}
            copyHookSnippet={model.copyHookSnippet}
          />
          {/* Two lists, two sections: the "Observed activity" heading only
           * repeated the tab above it, and its paragraph only described these
           * rows. */}
          <AgentBridgeSessions sessions={model.sessions} />
          <AgentBridgeRequests
            requests={model.requests}
            interactiveReady={model.interactiveReady}
            expiryTimeFormatter={model.expiryTimeFormatter}
            decidePermission={model.decidePermission}
            dismissRequest={model.dismissRequest}
          />
        </TabsContent>
        <TabsContent value="queue" className="flex flex-col gap-10 pt-8">
          <AgentBridgeQueue model={model} />
        </TabsContent>
        <TabsContent value="rules" className="pt-8">
          <AgentBridgeRules
            bridge={model.bridge}
            mutateBridge={model.mutateBridge}
          />
        </TabsContent>
      </Tabs>
    </div>
  );
};
