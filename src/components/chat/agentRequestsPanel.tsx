import React, { useEffect, useRef } from "react";
import { ChevronLeft, MessagesSquare } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/vg/button";

interface AgentRequestsSurfaceProps {
  bridgeEnabled: boolean;
  pendingCount: number;
  open: boolean;
  children: React.ReactNode;
  onOpen: () => void;
  onClose: () => void;
}

export const AgentRequestsSurface: React.FC<AgentRequestsSurfaceProps> = ({
  bridgeEnabled,
  pendingCount,
  open,
  children,
  onOpen,
  onClose,
}) => {
  const { t } = useTranslation();
  const entryButtonRef = useRef<HTMLButtonElement>(null);
  const wasOpen = useRef(open);

  useEffect(() => {
    if (wasOpen.current && !open) entryButtonRef.current?.focus();
    wasOpen.current = open;
  }, [open]);

  if (!bridgeEnabled || pendingCount === 0) return null;

  if (!open) {
    return (
      <div
        data-slot="agent-requests-entry"
        className="flex-none border-t border-gray-alpha-400 p-2"
      >
        <Button
          ref={entryButtonRef}
          type="button"
          variant="ghost"
          size="sm"
          className="w-full justify-between"
          onClick={onOpen}
        >
          <span className="flex min-w-0 items-center gap-2">
            <MessagesSquare aria-hidden="true" />
            <span className="truncate">
              {t("settings.agents.pending.title")}
            </span>
          </span>
          <span className="tabular-nums">{pendingCount}</span>
        </Button>
      </div>
    );
  }

  return (
    <div
      data-slot="agent-requests-panel"
      className="absolute inset-0 z-30 flex min-h-0 flex-col bg-background-100"
    >
      <div className="flex h-12 flex-none items-center gap-2 border-b border-gray-alpha-400 px-2">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          autoFocus
          onClick={onClose}
        >
          <ChevronLeft aria-hidden="true" />
          {t("meetings.actions.back")}
        </Button>
        <h2 className="truncate text-[14px] font-medium text-gray-1000">
          {t("settings.agents.pending.title")}
        </h2>
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto p-3">
        {children}
      </div>
    </div>
  );
};
