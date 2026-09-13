import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/vg/button";
import {
  Microlabel,
  Notice,
  SettingsSection,
} from "@/components/settings/rows";
import type { AgentBridgeSettingsModel } from "./useAgentBridgeSettings";

interface AgentBridgeRequestsProps {
  requests: AgentBridgeSettingsModel["requests"];
  interactiveReady: AgentBridgeSettingsModel["interactiveReady"];
  expiryTimeFormatter: AgentBridgeSettingsModel["expiryTimeFormatter"];
  decidePermission: AgentBridgeSettingsModel["decidePermission"];
  dismissRequest: AgentBridgeSettingsModel["dismissRequest"];
}

export const AgentBridgeRequests: React.FC<AgentBridgeRequestsProps> = ({
  requests,
  interactiveReady,
  expiryTimeFormatter,
  decidePermission,
  dismissRequest,
}) => {
  const { t } = useTranslation();

  return (
    <SettingsSection label={t("settings.agents.observed.requests")}>
      {requests.length === 0 ? (
        <div className="px-6 py-2.5">
          <Notice>{t("settings.agents.observed.noRequests")}</Notice>
        </div>
      ) : (
        requests.map((request) => {
          const agent = t(
            "settings.agents.controls.providers." + request.agent + ".label",
          );
          const open = request.state === "observed";
          /* The backend decides which invocations are holding their agent open
           * for an answer, so the console never has to keep its own list of
           * which providers and events can be replied to. */
          const permissionKind =
            request.kind === "permission_request" ||
            request.kind === "pre_tool_use";
          const canRespond =
            interactiveReady &&
            open &&
            permissionKind &&
            request.awaiting_response;
          const observeOnly =
            open && permissionKind && !request.awaiting_response;

          return (
            <div
              key={request.id}
              data-slot="agent-bridge-request"
              className="flex min-w-0 flex-wrap items-start justify-between gap-3 px-6 py-3"
            >
              <div className="min-w-0 flex-1">
                <p className="text-[14px] leading-[21px] break-words text-gray-1000">
                  {agent}
                  {" · "}
                  {t("settings.agents.observed.requestKinds." + request.kind)}
                  {request.tool_name ? " · " + request.tool_name : ""}
                </p>
                {request.tool_input_preview ? (
                  <pre className="mt-1 max-h-32 overflow-hidden font-mono text-[12px] leading-[18px] break-all whitespace-pre-wrap text-gray-900">
                    {request.tool_input_preview}
                  </pre>
                ) : null}
                <Microlabel className="mt-1 block">
                  {t("settings.agents.observed.expires", {
                    time: expiryTimeFormatter.format(
                      new Date(request.expires_at_ms),
                    ),
                  })}
                </Microlabel>
                {observeOnly ? (
                  <Notice className="mt-1">
                    {t("settings.agents.observed.observeOnly", { agent })}
                  </Notice>
                ) : null}
              </div>
              <div className="flex shrink-0 flex-wrap gap-2">
                {canRespond ? (
                  <>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => void decidePermission(request, "allow")}
                    >
                      {t("settings.agents.observed.allowExact")}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      className="text-red-900"
                      onClick={() => void decidePermission(request, "deny")}
                    >
                      {t("settings.agents.observed.denyExact")}
                    </Button>
                  </>
                ) : null}
                {open ? (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void dismissRequest(request.id)}
                  >
                    {t("settings.agents.observed.dismiss")}
                  </Button>
                ) : null}
              </div>
            </div>
          );
        })
      )}
    </SettingsSection>
  );
};
