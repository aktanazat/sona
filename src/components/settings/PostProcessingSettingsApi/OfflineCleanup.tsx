import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { commands, type CleanupStatus } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { formatModelSize } from "@/lib/utils/format";
import { Notice, SettingsField } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";

type CleanupAction = "download" | "cancel" | "remove" | "use";

export const OfflineCleanup = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const [status, setStatus] = useState<CleanupStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const activeMode = settings?.modes?.find(
    (mode) => mode.id === settings.active_mode_id,
  );
  const selected =
    (activeMode?.llm.provider_id ?? settings?.post_process_provider_id) ===
      "sona_local" &&
    activeMode?.llm.enabled === true &&
    activeMode.llm.cleanup_level === "light";

  useEffect(() => {
    let disposed = false;
    let sawEvent = false;
    let unlisten: UnlistenFn | undefined;
    void (async () => {
      try {
        const cleanup = await listen<CleanupStatus>(
          "local-cleanup-state",
          ({ payload }) => {
            if (disposed) return;
            sawEvent = true;
            setStatus(payload);
          },
        );
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
        const result = await commands.getLocalCleanupStatus();
        if (disposed) return;
        if (result.status === "error") setError(result.error);
        else if (!sawEvent) setStatus(result.data);
      } catch (failure) {
        if (!disposed) setError(String(failure));
      }
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const run = async (action: CleanupAction) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await (() => {
        switch (action) {
          case "download":
            return commands.downloadLocalCleanup();
          case "cancel":
            return commands.cancelLocalCleanupDownload();
          case "remove":
            return commands.removeLocalCleanup();
          case "use":
            return commands.selectLocalCleanup();
        }
      })();
      if (result.status === "error") {
        setError(result.error);
        return;
      }
      if (action === "use") await refreshSettings();
      if (action === "remove") setConfirmRemove(false);
      const latest = await commands.getLocalCleanupStatus();
      if (latest.status === "error") setError(latest.error);
      else setStatus(latest.data);
    } catch (failure: unknown) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  };

  const downloading = status?.phase === "downloading";
  const ready = status?.phase === "ready";
  const progress = status
    ? t("settings.postProcessing.offlineCleanup.downloading", {
        downloaded:
          status.downloaded_bytes === 0
            ? "0 MB"
            : formatModelSize(status.downloaded_bytes / 1_000_000),
        total: formatModelSize(status.size_bytes / 1_000_000),
      })
    : "";

  return (
    <>
      <SettingsField
        label={t("settings.postProcessing.offlineCleanup.title")}
        hint={
          status
            ? t("settings.postProcessing.offlineCleanup.description", {
                language: status.language,
                size: formatModelSize(status.size_bytes / 1_000_000),
              })
            : undefined
        }
      >
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <span className="text-sm text-gray-900" role="status">
              {downloading
                ? progress
                : ready
                  ? t("settings.postProcessing.offlineCleanup.ready")
                  : status
                    ? t("settings.postProcessing.offlineCleanup.notDownloaded")
                    : t("settings.postProcessing.api.model.status.loading")}
            </span>
            <div className="flex flex-wrap items-center gap-2">
              {downloading ? (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  onClick={() => void run("cancel")}
                >
                  {t("settings.postProcessing.offlineCleanup.cancel")}
                </Button>
              ) : ready ? (
                <>
                  <Button
                    size="sm"
                    disabled={busy || selected || !status.runtime_available}
                    onClick={() => void run("use")}
                  >
                    {t(
                      selected
                        ? "settings.postProcessing.offlineCleanup.selected"
                        : "settings.postProcessing.offlineCleanup.use",
                    )}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => setConfirmRemove(true)}
                  >
                    {t("settings.postProcessing.offlineCleanup.remove")}
                  </Button>
                </>
              ) : (
                <Button
                  size="sm"
                  disabled={busy || !status?.runtime_available}
                  onClick={() => void run("download")}
                >
                  {t(
                    status?.phase === "failed"
                      ? "settings.postProcessing.offlineCleanup.retry"
                      : "settings.postProcessing.offlineCleanup.download",
                  )}
                </Button>
              )}
            </div>
          </div>
          {downloading && status ? (
            <progress
              className="h-1 w-full accent-accent-strong"
              value={status.downloaded_bytes}
              max={status.size_bytes}
              aria-label={t("settings.postProcessing.offlineCleanup.title")}
              aria-valuetext={progress}
            />
          ) : null}
          {status && !status.runtime_available ? (
            <Notice tone="warning">
              {error ||
                status.error ||
                t("settings.postProcessing.offlineCleanup.runtimeMissing")}
            </Notice>
          ) : null}
          {status?.runtime_available !== false && (error || status?.error) ? (
            <Notice tone="danger">{error || status?.error}</Notice>
          ) : null}
        </div>
      </SettingsField>
      <Dialog open={confirmRemove} onOpenChange={setConfirmRemove}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {t("settings.postProcessing.offlineCleanup.removeTitle")}
            </DialogTitle>
            <DialogDescription>
              {t("settings.postProcessing.offlineCleanup.removeDescription")}
            </DialogDescription>
          </DialogHeader>
          {error ? <Notice tone="danger">{error}</Notice> : null}
          <DialogFooter>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => setConfirmRemove(false)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              size="sm"
              disabled={busy}
              onClick={() => void run("remove")}
            >
              {t("settings.postProcessing.offlineCleanup.remove")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
};
