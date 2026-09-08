import { useCallback, useEffect, useMemo, useState } from "react";
import { useSettings } from "@/hooks/useSettings";
import {
  commands,
  type PostProcessProvider,
  type PostProcessProviderConsent,
  type SecretState,
} from "@/bindings";
import type { ModelOption, ProviderOption } from "./types";
import { usePostProcessModelCatalog } from "./usePostProcessModelCatalog";

export type PostProcessProviderState = {
  providerOptions: ProviderOption[];
  selectedProviderId: string;
  selectedProvider: PostProcessProvider | undefined;
  isCustomProvider: boolean;
  isAppleProvider: boolean;
  appleIntelligenceUnavailable: boolean;
  baseUrl: string;
  handleBaseUrlChange: (value: string) => void;
  isBaseUrlUpdating: boolean;
  secretState: SecretState | undefined;
  handleSecretCommit: (value: string) => Promise<boolean>;
  handleSecretDelete: () => void;
  isSecretUpdating: boolean;
  isSecretUnavailable: boolean;
  model: string;
  modelOptions: ModelOption[];
  modelStatusKeys: string[];
  localEndpointUnavailable: boolean;
  allowsManualModelId: boolean;
  isModelUpdating: boolean;
  isFetchingModels: boolean;
  remoteConsent: PostProcessProviderConsent | undefined;
  handleProviderSelect: (providerId: string) => void;
  handleModelSelect: (value: string) => void;
  handleModelCreate: (value: string) => void;
  handleRefreshModels: () => void;
};

const APPLE_PROVIDER_ID = "apple_intelligence";
const EMPTY_POST_PROCESS_PROVIDERS: PostProcessProvider[] = [];

export const usePostProcessProviderState = (): PostProcessProviderState => {
  const {
    settings,
    isUpdating,
    setPostProcessProvider,
    updatePostProcessBaseUrl,
    replacePostProcessSecret,
    removePostProcessSecret,
    refreshPostProcessSecretState,
    updatePostProcessModel,
  } = useSettings();

  const providers =
    settings?.post_process_providers ?? EMPTY_POST_PROCESS_PROVIDERS;
  const selectedProviderId = useMemo(
    () => settings?.post_process_provider_id || providers[0]?.id || "openai",
    [providers, settings?.post_process_provider_id],
  );
  const selectedProvider = useMemo(
    () => providers.find((provider) => provider.id === selectedProviderId),
    [providers, selectedProviderId],
  );
  const isKnownSelectedProvider = selectedProvider !== undefined;
  const isAppleProvider = selectedProvider?.id === APPLE_PROVIDER_ID;
  const [appleIntelligenceUnavailable, setAppleIntelligenceUnavailable] =
    useState(false);
  const baseUrl = selectedProvider?.base_url ?? "";
  const secretState =
    settings?.post_process_secret_states?.[selectedProviderId];
  const remoteConsent =
    settings?.post_process_provider_consents?.[selectedProviderId];
  const model = settings?.post_process_models?.[selectedProviderId] ?? "";

  useEffect(() => {
    if (isKnownSelectedProvider && !isAppleProvider) {
      void refreshPostProcessSecretState(selectedProviderId);
    }
  }, [
    isAppleProvider,
    isKnownSelectedProvider,
    refreshPostProcessSecretState,
    selectedProviderId,
  ]);

  const providerOptions = useMemo<ProviderOption[]>(
    () =>
      providers.map((provider) => ({
        value: provider.id,
        label: provider.label,
      })),
    [providers],
  );
  const discoveryReady =
    isKnownSelectedProvider &&
    Boolean(baseUrl.trim()) &&
    (selectedProvider?.id === "custom" || secretState !== undefined);
  const automaticDiscoveryKey = [
    remoteConsent?.text_transfer_consent ? "consented" : "needs-consent",
    secretState?.configured === true
      ? "configured"
      : secretState?.configured === false
        ? "missing"
        : "unknown",
    secretState?.lastErrorKind ?? "none",
  ].join("\u0000");
  const modelCatalog = usePostProcessModelCatalog({
    autoDiscover: discoveryReady && !isAppleProvider,
    autoDiscoveryKey: automaticDiscoveryKey,
    baseUrl,
    enabled: isKnownSelectedProvider && !isAppleProvider,
    providerId: selectedProviderId,
    savedModelId: model,
  });

  const handleProviderSelect = useCallback(
    async (providerId: string) => {
      setAppleIntelligenceUnavailable(false);
      if (providerId === selectedProviderId) return;

      if (providerId === APPLE_PROVIDER_ID) {
        const available = await commands.checkAppleIntelligenceAvailable();
        if (!available) setAppleIntelligenceUnavailable(true);
      }
      await setPostProcessProvider(providerId);
    },
    [selectedProviderId, setPostProcessProvider],
  );

  const handleBaseUrlChange = useCallback(
    (value: string) => {
      if (selectedProvider?.id !== "custom") return;
      const trimmed = value.trim();
      if (trimmed && trimmed !== baseUrl) {
        void updatePostProcessBaseUrl(selectedProvider.id, trimmed);
      }
    },
    [baseUrl, selectedProvider, updatePostProcessBaseUrl],
  );

  const handleSecretCommit = useCallback(
    (value: string) => replacePostProcessSecret(selectedProviderId, value),
    [replacePostProcessSecret, selectedProviderId],
  );
  const handleSecretDelete = useCallback(() => {
    void removePostProcessSecret(selectedProviderId);
  }, [removePostProcessSecret, selectedProviderId]);
  const handleModelSelect = useCallback(
    (value: string) => {
      void updatePostProcessModel(selectedProviderId, value.trim());
    },
    [selectedProviderId, updatePostProcessModel],
  );
  const handleModelCreate = useCallback(
    (value: string) => {
      void updatePostProcessModel(selectedProviderId, value.trim());
    },
    [selectedProviderId, updatePostProcessModel],
  );
  /* The catalog hook returns a fresh view object on every render, so this
   * depends on its `discover` callback, which is stable, rather than on the
   * view that carries it. */
  const discoverModelCatalog = modelCatalog.discover;
  const handleRefreshModels = useCallback(() => {
    if (!isKnownSelectedProvider || isAppleProvider) return;
    void (async () => {
      await refreshPostProcessSecretState(selectedProviderId);
      await discoverModelCatalog();
    })();
  }, [
    discoverModelCatalog,
    isAppleProvider,
    isKnownSelectedProvider,
    refreshPostProcessSecretState,
    selectedProviderId,
  ]);

  const localEndpointUnavailable =
    selectedProvider?.id === "custom" &&
    modelCatalog.statusKeys.includes(
      "settings.postProcessing.api.model.status.unreachable",
    );
  return {
    providerOptions,
    selectedProviderId,
    selectedProvider,
    isCustomProvider: selectedProvider?.id === "custom",
    isAppleProvider,
    appleIntelligenceUnavailable,
    baseUrl,
    handleBaseUrlChange,
    isBaseUrlUpdating: isUpdating(
      `post_process_base_url:${selectedProviderId}`,
    ),
    secretState,
    handleSecretCommit,
    handleSecretDelete,
    isSecretUpdating: isUpdating(`post_process_secret:${selectedProviderId}`),
    isSecretUnavailable: secretState?.lastErrorKind === "unavailable",
    model,
    modelOptions: modelCatalog.modelOptions,
    localEndpointUnavailable,
    modelStatusKeys: localEndpointUnavailable
      ? modelCatalog.statusKeys.filter(
          (key) =>
            key !== "settings.postProcessing.api.model.status.unreachable",
        )
      : modelCatalog.statusKeys,
    allowsManualModelId: modelCatalog.allowsManualModelId,
    isModelUpdating: isUpdating(`post_process_model:${selectedProviderId}`),
    isFetchingModels: modelCatalog.isLoading,
    remoteConsent,
    handleProviderSelect,
    handleModelSelect,
    handleModelCreate,
    handleRefreshModels,
  };
};
