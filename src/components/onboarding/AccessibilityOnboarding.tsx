import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { platform } from "@tauri-apps/plugin-os";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  checkAccessibilityPermission,
  requestAccessibilityPermission,
  checkMicrophonePermission,
  requestMicrophonePermission,
} from "tauri-plugin-macos-permissions-api";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { Button } from "@/components/vg/button";
import { SonaMark } from "../icons/SonaMark";
import { SonaWordmark } from "../icons/SonaWordmark";
import "./onboarding.css";

interface AccessibilityOnboardingProps {
  onComplete: () => void;
}

type PermissionStatus = "checking" | "needed" | "waiting" | "granted";
type PermissionPlatform = "macos" | "windows" | "other";

interface PermissionsState {
  accessibility: PermissionStatus;
  microphone: PermissionStatus;
}

/**
 * The exact System Settings pane for each permission.
 *
 * macOS shows the microphone consent dialog once, ever. After a denial
 * `requestMicrophonePermission()` resolves silently and the row would sit on a
 * spinner with no way out, which is the failure this screen was reported for.
 * Deep-linking the pane is the escape hatch; the two URLs are the entire
 * `opener` scope added in capabilities/default.json.
 */
const MACOS_SETTINGS_PANE = {
  microphone:
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
  accessibility:
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
} as const;

interface PermissionRowProps {
  title: string;
  description: string;
  /** Never "granted": a granted permission has no row to be in. */
  status: Exclude<PermissionStatus, "granted">;
  grantLabel: string;
  onGrant: () => void;
  onOpenSettings: () => void;
  onRecheck: () => void;
}

/**
 * One permission, as a flat section: what it is, why it is needed, and the one
 * button that grants it. A granted permission is not rendered at all, and the
 * prop type says so rather than the two call sites saying so twice: the screen
 * asks for what is still missing, and the last grant advances it.
 *
 * `waiting` is the state this screen used to get stuck in: a spinner with
 * nothing to click. It keeps two buttons rather than one against the round-7
 * rule, because after a macOS denial the consent dialog never appears again:
 * the exact System Settings pane and a re-check that restarts the poll are the
 * only two ways out, and MAX_POLLING_ERRORS can stop the poll for good.
 */
const PermissionRow: React.FC<PermissionRowProps> = ({
  title,
  description,
  status,
  grantLabel,
  onGrant,
  onOpenSettings,
  onRecheck,
}) => {
  const { t } = useTranslation();
  const waiting = status === "waiting";
  /* Both rows say "Grant permission", because the row's own title is what the
   * button is granting. Tab lands on the button without reading the row, so
   * the group carries the title to anyone who arrives that way. */
  const titleId = useId();

  return (
    <div className="ob-row" role="group" aria-labelledby={titleId}>
      <div className="ob-row-head">
        <h3 className="ob-row-title" id={titleId}>
          {title}
        </h3>
        {/* A word, not a chip, and only while the reader still has something to
         * do about it: the grant is happening somewhere else, in a window this
         * app does not own. */}
        {waiting && (
          <span className="ob-row-state" role="status">
            {t("onboarding.permissions.waiting")}
          </span>
        )}
      </div>
      <p className="ob-row-description">{description}</p>
      {waiting ? (
        <div className="ob-row-actions">
          <Button variant="outline" size="sm" onClick={onOpenSettings}>
            {t("accessibility.openSettings")}
          </Button>
          <Button variant="ghost" size="sm" onClick={onRecheck}>
            {t("onboarding.permissions.recheck")}
          </Button>
        </div>
      ) : (
        <div className="ob-row-actions">
          <Button size="sm" onClick={onGrant}>
            {grantLabel}
          </Button>
        </div>
      )}
    </div>
  );
};

interface PermissionOnboardingViewState {
  isChecking: boolean;
  allGranted: boolean;
  showMicrophonePermission: boolean;
  showAccessibilityPermission: boolean;
  isWindows: boolean;
}

interface PermissionOnboardingContentProps {
  view: PermissionOnboardingViewState;
  microphoneStatus: PermissionStatus;
  accessibilityStatus: PermissionStatus;
  onGrantMicrophone: () => void;
  onGrantAccessibility: () => void;
  onOpenMicrophoneSettings: () => void;
  onOpenAccessibilitySettings: () => void;
  onRecheck: () => void;
}

const OnboardingBrand: React.FC = () => (
  <div className="ob-brand">
    <SonaMark width={22} height={22} />
    <SonaWordmark className="text-[14px]" />
  </div>
);

const PermissionOnboardingContent: React.FC<
  PermissionOnboardingContentProps
> = ({
  view,
  microphoneStatus,
  accessibilityStatus,
  onGrantMicrophone,
  onGrantAccessibility,
  onOpenMicrophoneSettings,
  onOpenAccessibilitySettings,
  onRecheck,
}) => {
  const {
    isChecking,
    allGranted,
    showMicrophonePermission,
    showAccessibilityPermission,
    isWindows,
  } = view;
  const { t } = useTranslation();

  if (isChecking) {
    return (
      <div className="onboarding-shell ob-stage">
        <div className="ob-column">
          <OnboardingBrand />
          <span className="ob-checking" role="status">
            <span className="ob-spinner" aria-hidden="true" />
            {t("onboarding.permissions.checking")}
          </span>
        </div>
      </div>
    );
  }

  if (allGranted) {
    return (
      <div className="onboarding-shell ob-stage">
        <div className="ob-column">
          <OnboardingBrand />
          <h1 className="ob-headline">
            {t("onboarding.permissions.allGranted")}
          </h1>
        </div>
      </div>
    );
  }

  return (
    <div className="onboarding-shell ob-stage">
      <div className="ob-column">
        <OnboardingBrand />
        <h1 className="ob-headline">{t("onboarding.permissions.headline")}</h1>
        <p className="ob-subhead">{t("onboarding.permissions.subhead")}</p>

        <div className="ob-rows">
          {showMicrophonePermission && microphoneStatus !== "granted" && (
            <PermissionRow
              title={t("onboarding.permissions.microphone.title")}
              description={t("onboarding.permissions.microphone.description")}
              status={microphoneStatus}
              grantLabel={
                isWindows
                  ? t("accessibility.openSettings")
                  : t("onboarding.permissions.grant")
              }
              onGrant={onGrantMicrophone}
              onOpenSettings={onOpenMicrophoneSettings}
              onRecheck={onRecheck}
            />
          )}

          {showAccessibilityPermission && accessibilityStatus !== "granted" && (
            <PermissionRow
              title={t("onboarding.permissions.accessibility.title")}
              description={t(
                "onboarding.permissions.accessibility.description",
              )}
              status={accessibilityStatus}
              grantLabel={t("onboarding.permissions.grant")}
              onGrant={onGrantAccessibility}
              onOpenSettings={onOpenAccessibilitySettings}
              onRecheck={onRecheck}
            />
          )}
        </div>
      </div>
    </div>
  );
};

const AccessibilityOnboarding: React.FC<AccessibilityOnboardingProps> = ({
  onComplete,
}) => {
  const { t } = useTranslation();
  const refreshAudioDevices = useSettingsStore(
    (state) => state.refreshAudioDevices,
  );
  const refreshOutputDevices = useSettingsStore(
    (state) => state.refreshOutputDevices,
  );
  const [permissionPlatform, setPermissionPlatform] =
    useState<PermissionPlatform | null>(null);
  const [permissions, setPermissions] = useState<PermissionsState>({
    accessibility: "checking",
    microphone: "checking",
  });
  const pollingRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const errorCountRef = useRef<number>(0);
  const accessibilityGrantedRef = useRef(false);
  const MAX_POLLING_ERRORS = 3;

  const isMacOS = permissionPlatform === "macos";
  const isWindows = permissionPlatform === "windows";
  const showMicrophonePermission = isMacOS || isWindows;
  const showAccessibilityPermission = isMacOS;

  const allGranted = isMacOS
    ? permissions.accessibility === "granted" &&
      permissions.microphone === "granted"
    : isWindows
      ? permissions.microphone === "granted"
      : true;

  /* The mount probe below must not re-run because a parent re-rendered. App
   * builds a fresh `onComplete` on every one of its own renders, and this
   * screen's completion writes the device lists into the settings store -
   * which re-renders App. Depending on the prop's identity made those two
   * facts a loop on the granted path: probe, complete, store write, new prop,
   * probe again, until React stopped it with "Maximum update depth exceeded"
   * and the probe's own catch blamed the permission check for it - a new
   * install landed on the model step under "Couldn't check permissions". The
   * ref carries the latest callback without making its identity a reason to
   * run, which also holds the poll and its re-check still. */
  const onCompleteRef = useRef(onComplete);
  useEffect(() => {
    onCompleteRef.current = onComplete;
  }, [onComplete]);

  const completeOnboarding = useCallback(async () => {
    await Promise.all([refreshAudioDevices(), refreshOutputDevices()]);
    timeoutRef.current = setTimeout(() => onCompleteRef.current(), 300);
  }, [refreshAudioDevices, refreshOutputDevices]);

  const hasWindowsMicrophoneAccess = useCallback(async (): Promise<boolean> => {
    const microphoneStatus =
      await commands.getWindowsMicrophonePermissionStatus();

    if (!microphoneStatus.supported) {
      return true;
    }

    return microphoneStatus.overall_access !== "denied";
  }, []);

  // Enigo and the global shortcut listener are initialized exactly once per
  // accessibility grant, and the polled permission value is the only
  // transition source. This must stay outside every setState updater: React
  // may replay an updater, and replaying it once fired hundreds of IPC calls
  // in a second.
  const syncAccessibilityGrant = useCallback(
    async (granted: boolean): Promise<void> => {
      const wasGranted = accessibilityGrantedRef.current;
      accessibilityGrantedRef.current = granted;
      if (!granted || wasGranted) return;

      try {
        await Promise.all([
          commands.initializeEnigo(),
          commands.initializeShortcuts(),
        ]);
      } catch (e) {
        console.warn("Failed to initialize after permission grant:", e);
      }
    },
    [],
  );

  // Check platform and permission status on mount
  useEffect(() => {
    let cancelled = false;
    const currentPlatform = platform();
    const nextPlatform: PermissionPlatform =
      currentPlatform === "macos"
        ? "macos"
        : currentPlatform === "windows"
          ? "windows"
          : "other";

    setPermissionPlatform(nextPlatform);

    // Skip immediately on unsupported platforms
    if (nextPlatform === "other") {
      onCompleteRef.current();
      return;
    }

    const checkInitial = async () => {
      if (nextPlatform === "macos") {
        try {
          const [accessibilityGranted, microphoneGranted] = await Promise.all([
            checkAccessibilityPermission(),
            checkMicrophonePermission(),
          ]);
          if (cancelled) return;

          // Initialize Enigo and shortcuts when accessibility is granted
          await syncAccessibilityGrant(accessibilityGranted);

          if (cancelled) return;
          const newState: PermissionsState = {
            accessibility: accessibilityGranted ? "granted" : "needed",
            microphone: microphoneGranted ? "granted" : "needed",
          };

          setPermissions(newState);

          if (!cancelled && accessibilityGranted && microphoneGranted) {
            await completeOnboarding();
          }
        } catch (error) {
          console.error("Failed to check macOS permissions:", error);
          if (cancelled) return;
          toast.error(t("onboarding.permissions.errors.checkFailed"));
          setPermissions({
            accessibility: "needed",
            microphone: "needed",
          });
        }

        return;
      }

      try {
        const microphoneGranted = await hasWindowsMicrophoneAccess();
        if (cancelled) return;

        setPermissions({
          accessibility: "granted",
          microphone: microphoneGranted ? "granted" : "needed",
        });

        if (!cancelled && microphoneGranted) {
          await completeOnboarding();
        }
      } catch (error) {
        console.warn("Failed to check Windows microphone permissions:", error);
        if (cancelled) return;
        setPermissions({
          accessibility: "granted",
          microphone: "granted",
        });
        await completeOnboarding();
      }
    };

    void checkInitial();
    return () => {
      cancelled = true;
    };
  }, [
    completeOnboarding,
    hasWindowsMicrophoneAccess,
    syncAccessibilityGrant,
    t,
  ]);

  // Polling for permissions after user clicks a button
  const startPolling = useCallback(() => {
    if (pollingRef.current || permissionPlatform === null) return;

    pollingRef.current = setInterval(async () => {
      try {
        if (permissionPlatform === "windows") {
          const microphoneGranted = await hasWindowsMicrophoneAccess();

          if (microphoneGranted) {
            setPermissions((prev) => ({ ...prev, microphone: "granted" }));

            if (pollingRef.current) {
              clearInterval(pollingRef.current);
              pollingRef.current = null;
            }

            await completeOnboarding();
          }

          errorCountRef.current = 0;
          return;
        }

        const [accessibilityGranted, microphoneGranted] = await Promise.all([
          checkAccessibilityPermission(),
          checkMicrophonePermission(),
        ]);

        await syncAccessibilityGrant(accessibilityGranted);

        setPermissions((prev) => ({
          accessibility: accessibilityGranted ? "granted" : prev.accessibility,
          microphone: microphoneGranted ? "granted" : prev.microphone,
        }));

        // If both granted, stop polling, refresh audio devices, and proceed
        if (accessibilityGranted && microphoneGranted) {
          if (pollingRef.current) {
            clearInterval(pollingRef.current);
            pollingRef.current = null;
          }
          await completeOnboarding();
        }

        // Reset error count on success
        errorCountRef.current = 0;
      } catch (error) {
        console.error("Error checking permissions:", error);
        errorCountRef.current += 1;

        if (errorCountRef.current >= MAX_POLLING_ERRORS) {
          // Stop polling after too many consecutive errors
          if (pollingRef.current) {
            clearInterval(pollingRef.current);
            pollingRef.current = null;
          }
          toast.error(t("onboarding.permissions.errors.checkFailed"));
        }
      }
    }, 1000);
  }, [
    completeOnboarding,
    hasWindowsMicrophoneAccess,
    permissionPlatform,
    syncAccessibilityGrant,
    t,
  ]);

  // Cleanup polling and timeouts on unmount
  useEffect(() => {
    return () => {
      if (pollingRef.current) {
        clearInterval(pollingRef.current);
      }
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
    };
  }, []);

  const handleGrantAccessibility = async () => {
    try {
      await requestAccessibilityPermission();
      setPermissions((prev) => ({ ...prev, accessibility: "waiting" }));
      startPolling();
    } catch (error) {
      console.error("Failed to request accessibility permission:", error);
      toast.error(t("onboarding.permissions.errors.requestFailed"));
    }
  };

  const handleGrantMicrophone = async () => {
    try {
      if (isWindows) {
        await commands.openMicrophonePrivacySettings();
      } else {
        await requestMicrophonePermission();
      }

      setPermissions((prev) => ({ ...prev, microphone: "waiting" }));
      startPolling();
    } catch (error) {
      console.error("Failed to request microphone permission:", error);
      toast.error(t("onboarding.permissions.errors.requestFailed"));
    }
  };

  /* Restart the poll with a fresh error budget. The interval body above is
   * reused verbatim: this only guarantees a live poll, which matters because
   * MAX_POLLING_ERRORS consecutive failures stop it for good and leave the row
   * waiting on a check that will never run again. */
  const handleRecheck = useCallback(() => {
    if (pollingRef.current) {
      clearInterval(pollingRef.current);
      pollingRef.current = null;
    }
    errorCountRef.current = 0;
    startPolling();
  }, [startPolling]);

  const openSettingsPane = async (
    pane: keyof typeof MACOS_SETTINGS_PANE,
  ): Promise<void> => {
    try {
      if (isWindows) {
        await commands.openMicrophonePrivacySettings();
      } else {
        await openUrl(MACOS_SETTINGS_PANE[pane]);
      }
      // Settings is open; make sure something is watching for the flip.
      handleRecheck();
    } catch (error) {
      console.error("Failed to open the permission settings pane:", error);
      toast.error(t("onboarding.permissions.errors.requestFailed"));
    }
  };

  const isChecking =
    permissionPlatform === null ||
    (isMacOS &&
      permissions.accessibility === "checking" &&
      permissions.microphone === "checking") ||
    (isWindows && permissions.microphone === "checking");

  return (
    <PermissionOnboardingContent
      view={{
        isChecking,
        allGranted,
        showMicrophonePermission,
        showAccessibilityPermission,
        isWindows,
      }}
      microphoneStatus={permissions.microphone}
      accessibilityStatus={permissions.accessibility}
      onGrantMicrophone={handleGrantMicrophone}
      onGrantAccessibility={handleGrantAccessibility}
      onOpenMicrophoneSettings={() => void openSettingsPane("microphone")}
      onOpenAccessibilitySettings={() => void openSettingsPane("accessibility")}
      onRecheck={handleRecheck}
    />
  );
};

export default AccessibilityOnboarding;
