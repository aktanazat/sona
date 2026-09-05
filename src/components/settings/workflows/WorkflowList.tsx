import React from "react";
import { useTranslation } from "react-i18next";
import type { WorkflowId, WorkflowsListResult } from "@/bindings";
import { formatRelativeTime } from "@/lib/utils/format";
import { Button } from "@/components/vg/button";
import { Skeleton } from "@/components/vg/skeleton";
import { Switch } from "@/components/vg/switch";
import { Notice, SettingsSection } from "@/components/settings/rows";
import { WorkflowStatusGlyph } from "./WorkflowStatusGlyph";
import { formatWorkflowOutcome } from "./formatWorkflowOutcome";
import {
  WORKFLOW_DESCRIPTION_KEY,
  WORKFLOW_NAME_KEY,
} from "./workflowCatalogue";

interface WorkflowListProps {
  data: WorkflowsListResult | null;
  loading: boolean;
  error: boolean;
  pendingWorkflowId: WorkflowId | null;
  onRetry: () => void;
  onToggle: (workflowId: WorkflowId, enabled: boolean) => void;
  nowMs?: number;
}

export const WorkflowList: React.FC<WorkflowListProps> = ({
  data,
  loading,
  error,
  pendingWorkflowId,
  onRetry,
  onToggle,
  nowMs = Date.now(),
}) => {
  const { t } = useTranslation();

  return (
    <SettingsSection label={t("settingsV2.advanced.workflows")}>
      {loading && data === null ? (
        <div
          role="status"
          aria-label={t("common.loading")}
          className="space-y-3 px-6 py-3"
        >
          <Skeleton className="h-11 w-full" />
          <Skeleton className="h-11 w-full" />
          <Skeleton className="h-11 w-full" />
        </div>
      ) : error && data === null ? (
        <div className="flex flex-wrap items-center justify-between gap-3 px-6 py-3">
          <Notice tone="danger">{t("settings.workflows.loadError")}</Notice>
          <Button type="button" size="sm" variant="outline" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        </div>
      ) : (
        <ul role="list" className="divide-y divide-gray-alpha-400">
          {data?.entries.map((workflow) => {
            const name = t(WORKFLOW_NAME_KEY[workflow.id]);
            const description = WORKFLOW_DESCRIPTION_KEY[workflow.id];
            return (
              <li
                key={workflow.id}
                className="flex min-h-[76px] items-center justify-between gap-6 px-6 py-3"
              >
                <div className="min-w-0 space-y-1">
                  <p className="truncate text-[14px] font-medium text-gray-1000">
                    {name}
                  </p>
                  {description === undefined ? null : (
                    <p className="truncate text-[12.5px] text-gray-800">
                      {t(description)}
                    </p>
                  )}
                  {workflow.last_run === null ? (
                    <p className="text-[12px] text-gray-700">
                      {t("settings.workflows.neverRun")}
                    </p>
                  ) : (
                    <p className="flex min-w-0 items-center gap-1.5 text-[12px] text-gray-700">
                      <WorkflowStatusGlyph status={workflow.last_run.status} />
                      <span className="truncate">
                        {formatWorkflowOutcome(workflow.last_run, t)}
                      </span>
                      <span aria-hidden="true">·</span>
                      <span className="shrink-0 tabular-nums">
                        {formatRelativeTime(
                          workflow.last_run.finished_at_utc_ms,
                          nowMs,
                        )}
                      </span>
                    </p>
                  )}
                </div>
                <Switch
                  checked={workflow.enabled}
                  disabled={pendingWorkflowId !== null}
                  aria-label={t("settings.workflows.toggleLabel", { name })}
                  onCheckedChange={(enabled) => onToggle(workflow.id, enabled)}
                />
              </li>
            );
          })}
        </ul>
      )}
      {error && data !== null ? (
        <div className="flex flex-wrap items-center justify-between gap-3 px-6 py-3">
          <Notice tone="danger">{t("settings.workflows.updateError")}</Notice>
          <Button type="button" size="sm" variant="outline" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        </div>
      ) : null}
    </SettingsSection>
  );
};
