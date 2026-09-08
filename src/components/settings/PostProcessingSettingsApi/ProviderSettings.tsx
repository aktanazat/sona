import React, { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Notice, SettingsField } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { useSettings } from "@/hooks/useSettings";
import { ApiKeyField } from "./ApiKeyField";
import { BaseUrlField } from "./BaseUrlField";
import { ModelField } from "./ModelField";
import { ProviderSelect } from "./ProviderSelect";
import { RemoteProviderConsent } from "./RemoteProviderConsent";
import { usePostProcessProviderState } from "./usePostProcessProviderState";

const PostProcessingSettingsApiComponent: React.FC = () => {
  const { t } = useTranslation();
  const state = usePostProcessProviderState();
  const { refreshSettings } = useSettings();
  const [endpointChanged, setEndpointChanged] = useState(false);
  const fieldId = useId();

  useEffect(() => {
    setEndpointChanged(false);
  }, [state.selectedProviderId]);

  const handleBaseUrlChange = (value: string) => {
    if (value.trim() !== state.baseUrl.trim()) setEndpointChanged(true);
    state.handleBaseUrlChange(value);
  };

  return (
    /* Advanced owns the disclosure; this is the flat provider form inside it. */
    <>
      <SettingsField
        label={t("settings.postProcessing.api.provider.title")}
        controlId={`${fieldId}-provider`}
      >
        <ProviderSelect
          id={`${fieldId}-provider`}
          options={state.providerOptions}
          value={state.selectedProviderId}
          onChange={state.handleProviderSelect}
        />
      </SettingsField>

      {state.isAppleProvider ? (
        state.appleIntelligenceUnavailable ? (
          <div className="px-6 py-3">
            <Notice tone="danger">
              {t("settings.postProcessing.api.appleIntelligence.unavailable")}
            </Notice>
          </div>
        ) : null
      ) : (
        <>
          {state.isCustomProvider ? (
            <>
              <SettingsField
                label={t("settings.postProcessing.api.baseUrl.title")}
                controlId={`${fieldId}-base-url`}
              >
                <BaseUrlField
                  id={`${fieldId}-base-url`}
                  value={state.baseUrl}
                  onBlur={handleBaseUrlChange}
                  placeholder={t(
                    "settings.postProcessing.api.baseUrl.placeholder",
                  )}
                  disabled={state.isBaseUrlUpdating}
                />
              </SettingsField>
              {state.localEndpointUnavailable ? (
                <div className="px-6 py-3">
                  <Notice tone="warning">
                    {t("settings.postProcessing.api.model.status.unreachable")}
                  </Notice>
                </div>
              ) : null}
            </>
          ) : null}

          <SettingsField
            label={t("settings.postProcessing.api.apiKey.title")}
            controlId={`${fieldId}-api-key`}
          >
            <div className="flex items-center gap-2">
              <ApiKeyField
                id={`${fieldId}-api-key`}
                onCommit={state.handleSecretCommit}
                placeholder={t(
                  "settings.postProcessing.api.apiKey.placeholder",
                )}
                disabled={state.isSecretUpdating || state.isSecretUnavailable}
              />
              {state.secretState?.configured ? (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="text-red-900"
                  onClick={state.handleSecretDelete}
                  disabled={state.isSecretUpdating}
                >
                  {t("common.delete")}
                </Button>
              ) : null}
            </div>
          </SettingsField>
        </>
      )}

      {!state.isAppleProvider ? (
        <RemoteProviderConsent
          provider={state.selectedProvider}
          consent={state.remoteConsent}
          endpointChanged={endpointChanged}
          onAccepted={refreshSettings}
        />
      ) : null}

      {state.isAppleProvider ? (
        <SettingsField label={t("settings.postProcessing.api.model.title")}>
          <span className="text-sm text-gray-900">
            {state.model || state.selectedProvider?.label}
          </span>
        </SettingsField>
      ) : (
        <SettingsField
          label={t("settings.postProcessing.api.model.title")}
          hint={
            state.isCustomProvider
              ? t("settings.postProcessing.api.model.descriptionCustom")
              : undefined
          }
          controlId={`${fieldId}-model`}
        >
          <ModelField
            id={`${fieldId}-model`}
            value={state.model}
            options={state.modelOptions}
            allowCustom={state.allowsManualModelId}
            disabled={state.isModelUpdating}
            isLoading={state.isFetchingModels}
            statusKeys={state.modelStatusKeys}
            onSelect={state.handleModelSelect}
            onCreate={state.handleModelCreate}
            onRefresh={state.handleRefreshModels}
          />
        </SettingsField>
      )}
    </>
  );
};

export const PostProcessingSettingsApi = React.memo(
  PostProcessingSettingsApiComponent,
);
PostProcessingSettingsApi.displayName = "PostProcessingSettingsApi";
