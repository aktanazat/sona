import { useCallback, useEffect, useMemo, useState } from "react";
import { commands, type CloudSttProvider, type SecretState } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import {
  CLOUD_STT_PROVIDERS,
  cloudSttProviderHasCurrentConsent,
} from "@/lib/cloudStt";
import { modeLlmDestination } from "@/components/settings/modes/modeModel";

/* Which routes off this Mac are live, read from the credential store rather
 * than inferred from settings: a provider is only a route once a key for it
 * exists. Two independent families — the post-processing providers a mode
 * sends text to, and the transcription providers audio can reach. */
export const useEgressRoutes = () => {
  const { settings } = useSettings();

  const cloudCandidateProviders = useMemo(() => {
    const enabledProviderIds = new Set<string>();
    for (const mode of settings?.modes ?? []) {
      /* A mode with no provider of its own sends to the global one, so the
       * route it opens is the global provider's. Reading the raw field here
       * would under-report a live destination on this screen. */
      if (mode.llm.enabled) {
        enabledProviderIds.add(
          modeLlmDestination(mode.llm, settings).providerId,
        );
      }
    }

    const candidates = [];
    for (const provider of settings?.post_process_providers ?? []) {
      if (
        !enabledProviderIds.has(provider.id) ||
        provider.id === "apple_intelligence" ||
        provider.id === "sona_local"
      ) {
        continue;
      }
      try {
        const hostname = new URL(provider.base_url).hostname.toLowerCase();
        if (
          hostname !== "localhost" &&
          hostname !== "127.0.0.1" &&
          hostname !== "::1"
        ) {
          candidates.push(provider);
        }
      } catch {
        candidates.push(provider);
      }
    }
    return candidates;
  }, [settings]);
  const [providerSecretStates, setProviderSecretStates] = useState<
    Record<string, boolean | undefined>
  >({});
  const [cloudSttSecretStates, setCloudSttSecretStates] = useState<
    Map<CloudSttProvider, SecretState>
  >(() => new Map());
  const [checkingCloudSttRoutes, setCheckingCloudSttRoutes] = useState(true);
  const [cloudSttRouteError, setCloudSttRouteError] = useState(false);

  useEffect(() => {
    let cancelled = false;
    if (cloudCandidateProviders.length === 0) {
      setProviderSecretStates({});
      return () => {
        cancelled = true;
      };
    }

    void Promise.all(
      cloudCandidateProviders.map(async (provider) => {
        try {
          const result = await commands.getProviderSecretState(
            "llm",
            provider.id,
          );
          return [
            provider.id,
            result.status === "ok" && result.data.configured,
          ] as const;
        } catch {
          return [provider.id, false] as const;
        }
      }),
    ).then((states) => {
      if (!cancelled) setProviderSecretStates(Object.fromEntries(states));
    });

    return () => {
      cancelled = true;
    };
  }, [cloudCandidateProviders, settings?.post_process_secret_states]);

  const configuredCloudProviders = useMemo(() => {
    const configured: string[] = [];
    for (const provider of cloudCandidateProviders) {
      if (providerSecretStates[provider.id] === true) {
        configured.push(provider.label);
      }
    }
    return configured;
  }, [cloudCandidateProviders, providerSecretStates]);
  const cloudRoutePending = cloudCandidateProviders.some(
    (provider) => providerSecretStates[provider.id] === undefined,
  );

  const [cloudSttAttempt, setCloudSttAttempt] = useState(0);
  // A failed secret-state read is transient (the credential store can be
  // locked), so the row offers a retry rather than a dead end.
  const retryCloudSttRoutes = useCallback(
    () => setCloudSttAttempt((current) => current + 1),
    [],
  );

  useEffect(() => {
    let cancelled = false;

    const loadCloudSttSecretStates = async () => {
      setCheckingCloudSttRoutes(true);
      setCloudSttRouteError(false);
      const results = await Promise.all(
        CLOUD_STT_PROVIDERS.map(async (provider) => {
          try {
            const result = await commands.getProviderSecretState(
              "stt",
              provider.secretAccountId,
            );
            return {
              provider: provider.provider,
              state: result.status === "ok" ? result.data : null,
              failed: result.status === "error",
            };
          } catch {
            return { provider: provider.provider, state: null, failed: true };
          }
        }),
      );
      if (cancelled) return;

      const next = new Map<CloudSttProvider, SecretState>();
      for (const result of results) {
        if (result.state) next.set(result.provider, result.state);
      }
      setCloudSttSecretStates(next);
      setCloudSttRouteError(results.some((result) => result.failed));
      setCheckingCloudSttRoutes(false);
    };

    void loadCloudSttSecretStates();
    return () => {
      cancelled = true;
    };
  }, [cloudSttAttempt, settings?.cloud_stt_providers]);

  const cloudSttDisclosureProviders = useMemo(
    () =>
      CLOUD_STT_PROVIDERS.filter(
        (provider) =>
          cloudSttSecretStates.get(provider.provider)?.configured &&
          cloudSttProviderHasCurrentConsent(
            settings?.cloud_stt_providers,
            provider.provider,
          ),
      ),
    [cloudSttSecretStates, settings?.cloud_stt_providers],
  );

  return {
    cloudRoutePending,
    configuredCloudProviders,
    checkingCloudSttRoutes,
    cloudSttRouteError,
    cloudSttDisclosureProviders,
    retryCloudSttRoutes,
  };
};
