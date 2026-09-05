import { useEffect, useState } from "react";
import {
  commands,
  type CloudSttProvider,
  type ModeDefinition,
  type RequestedEngine,
  type SecretState,
} from "@/bindings";
import {
  CLOUD_STT_PROVIDERS,
  cloudSttProviderForEngine,
  cloudSttProviderHasCurrentConsent,
  type CloudSttProviderMetadata,
} from "@/lib/cloudStt";
import { useSettings } from "@/hooks/useSettings";
import type { ModeCloudState, ModeDraftUpdaters } from "./modeModel";

/**
 * Which engine a mode asks for, and the one gate in front of the cloud ones.
 *
 * Two facts decide whether a cloud route is even offerable: a native key saved
 * in the keyring, probed once per mount, and current audio-transfer consent,
 * which lives in app settings. Neither is a mode setting, which is why the
 * draft never carries them and why selecting a cloud engine can pause on a
 * consent answer instead of writing straight through.
 *
 * The pause is state, not markup: this owns the pending provider, the in-flight
 * accept and the error a refused accept returns, and the editor renders the
 * dialog over them.
 */

/** Which half of an accept failed: the provider, or the call itself. */
export type CloudSttConsentError = "unknown_provider" | "backend";

export interface CloudSttEngineChoice {
  /** The resolved cloud answer every panel on the screen reads. */
  cloud: ModeCloudState;
  /** The provider whose consent is being asked for, or `null`. */
  pendingConsent: CloudSttProviderMetadata | null;
  consentError: CloudSttConsentError | null;
  accepting: boolean;
  acceptConsent: () => void;
  /** Decline, close or Escape: the ask is dropped and its error with it. */
  dismissConsent: () => void;
}

export const useCloudSttEngineChoice = (
  mode: ModeDefinition,
  updaters: ModeDraftUpdaters,
): CloudSttEngineChoice => {
  const { refreshSettings, settings } = useSettings();
  const [secretStates, setSecretStates] = useState<
    Partial<Record<CloudSttProvider, SecretState>>
  >({});
  const [pendingConsent, setPendingConsent] =
    useState<CloudSttProviderMetadata | null>(null);
  const [consentError, setConsentError] = useState<CloudSttConsentError | null>(
    null,
  );
  const [accepting, setAccepting] = useState(false);

  useEffect(() => {
    let cancelled = false;

    const loadCloudSecretStates = async () => {
      const next: Partial<Record<CloudSttProvider, SecretState>> = {};
      await Promise.all(
        CLOUD_STT_PROVIDERS.map(async (provider) => {
          try {
            const result = await commands.getProviderSecretState(
              "stt",
              provider.secretAccountId,
            );
            if (result.status === "ok") {
              next[provider.provider] = result.data;
            }
          } catch {
            // A failed keyring probe must not create a selectable cloud route.
          }
        }),
      );
      if (!cancelled) setSecretStates(next);
    };

    void loadCloudSecretStates();
    return () => {
      cancelled = true;
    };
  }, []);

  const requestedEngine = mode.asr.requested_engine ?? "local";
  const selectedProvider = cloudSttProviderForEngine(requestedEngine);
  const controlsAvailable =
    selectedProvider !== undefined &&
    secretStates[selectedProvider.provider]?.configured === true &&
    cloudSttProviderHasCurrentConsent(
      settings?.cloud_stt_providers,
      selectedProvider.provider,
    );

  const selectCloudEngine = (provider: CloudSttProviderMetadata) => {
    updaters.replace({
      ...mode,
      asr: {
        ...mode.asr,
        requested_engine: provider.provider,
        local_fallback_enabled: mode.asr.local_fallback_enabled ?? true,
        cloud_timestamps: true,
      },
    });
  };

  const selectEngine = (engine: RequestedEngine) => {
    if (engine === "local") {
      updaters.updateAsr("requested_engine", engine);
      return;
    }

    const provider = cloudSttProviderForEngine(engine);
    if (!provider || !secretStates[provider.provider]?.configured) {
      return;
    }
    if (
      !cloudSttProviderHasCurrentConsent(
        settings?.cloud_stt_providers,
        provider.provider,
      )
    ) {
      setConsentError(null);
      setPendingConsent(provider);
      return;
    }
    selectCloudEngine(provider);
  };

  const acceptConsent = async () => {
    if (!pendingConsent) return;

    setAccepting(true);
    setConsentError(null);
    try {
      const result = await commands.acceptCloudSttProviderConsent(
        pendingConsent.provider,
      );
      if (result.status === "ok") {
        await refreshSettings();
        selectCloudEngine(pendingConsent);
        setPendingConsent(null);
      } else {
        setConsentError(
          result.error === "unknown_provider" ? "unknown_provider" : "backend",
        );
      }
    } catch {
      setConsentError("backend");
    } finally {
      setAccepting(false);
    }
  };

  return {
    cloud: {
      requestedEngine,
      selectedProvider,
      isConfigured: (provider) => secretStates[provider]?.configured === true,
      controlsAvailable,
      selectEngine,
    },
    pendingConsent,
    consentError,
    accepting,
    acceptConsent: () => void acceptConsent(),
    dismissConsent: () => {
      setPendingConsent(null);
      setConsentError(null);
    },
  };
};
