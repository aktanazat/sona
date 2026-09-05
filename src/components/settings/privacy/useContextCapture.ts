import { useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type ContextPolicy } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";

/* The two writes that decide what Sona reads from other apps. */
export const useContextCapture = () => {
  const { t } = useTranslation();
  const { getSetting, refreshSettings } = useSettings();
  const [ceilingError, setCeilingError] = useState<string | null>(null);
  const [ceilingUpdating, setCeilingUpdating] = useState(false);
  const [urlCaptureError, setUrlCaptureError] = useState<string | null>(null);
  const [urlCaptureUpdating, setUrlCaptureUpdating] = useState(false);
  const contextCeiling = getSetting("context_policy_ceiling") ?? "none";
  const contextUrlCaptureEnabled =
    getSetting("context_url_capture_enabled") ?? false;

  const changeContextCeiling = async (ceiling: ContextPolicy) => {
    setCeilingUpdating(true);
    setCeilingError(null);
    try {
      const result = await commands.changeContextPolicyCeilingSetting(ceiling);
      if (result.status === "error") {
        setCeilingError(result.error);
        return;
      }
      await refreshSettings();
    } catch (error) {
      setCeilingError(String(error));
    } finally {
      setCeilingUpdating(false);
    }
  };

  const changeContextUrlCaptureEnabled = async (enabled: boolean) => {
    setUrlCaptureUpdating(true);
    setUrlCaptureError(null);
    try {
      const result =
        await commands.changeContextUrlCaptureEnabledSetting(enabled);
      if (result.status === "error") {
        setUrlCaptureError(t("settings.privacy.context.urlCapture.error"));
        return;
      }
      await refreshSettings();
    } catch {
      setUrlCaptureError(t("settings.privacy.context.urlCapture.error"));
    } finally {
      setUrlCaptureUpdating(false);
    }
  };

  return {
    contextCeiling,
    contextUrlCaptureEnabled,
    ceilingError,
    ceilingUpdating,
    urlCaptureError,
    urlCaptureUpdating,
    changeContextCeiling,
    changeContextUrlCaptureEnabled,
  };
};
