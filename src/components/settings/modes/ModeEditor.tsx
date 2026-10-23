import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type {
  ModeActivationRule,
  ModeDefinition,
  ModeView,
  ModeWebsiteActivationRule,
  ModelInfo,
  WebsiteHostMatch,
} from "@/bindings";
import { SELECTABLE_LANGUAGES } from "@/lib/constants/languages";
import { getTranslatedModelName } from "@/lib/utils/modelTranslation";
import { useSettings } from "@/hooks/useSettings";
import { Button } from "@/components/vg/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { Input } from "@/components/vg/input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { Switch } from "@/components/vg/switch";
import { Textarea } from "@/components/vg/textarea";
import {
  Notice,
  SettingsField,
  SettingsRow,
  SettingsSection,
} from "@/components/settings/rows";
import { ActivationRuleList, type ActivationRuleItem } from "./ModeControls";
import { ModeAdvanced } from "./ModeAdvanced";
import {
  PASTE_METHODS,
  PROMPT_PRESETS,
  REQUESTED_ENGINES,
  WEBSITE_HOST_MATCHES,
  createModeDraftUpdaters,
  downloadedModelOptions,
  modeDraftIsDirty,
  modeEngineOptions,
  modeLlmDestination,
} from "./modeModel";
import { useCloudSttEngineChoice } from "./useCloudSttEngineChoice";
import { useVocabularyRows } from "./useVocabularyRows";

export interface ModeEditorProps {
  mode: ModeDefinition;
  /** The persisted mode behind the draft, for the unsaved-change affordance. */
  savedMode: ModeView | undefined;
  modeCount: number;
  models: ModelInfo[];
  onChange: (mode: ModeDefinition) => void;
  onSave: () => void;
  saving: boolean;
  conflict: boolean;
  activationRules: readonly ModeActivationRule[];
  websiteActivationRules: readonly ModeWebsiteActivationRule[];
  activationSupported: boolean;
  capturingActivation: boolean;
  onCaptureActivation: () => void;
  onRemoveActivation: (appId: string) => void;
  onCaptureWebsiteActivation: (matchKind: WebsiteHostMatch) => void;
  onRemoveWebsiteActivation: (
    host: string,
    matchKind: WebsiteHostMatch,
  ) => void;
}

const NO_PROVIDERS: readonly [] = [];

/* A select item cannot carry an empty value, and "" is what the draft stores
 * for "inherit the global model". The sentinel lives between this Select and
 * its handler only: "" is still what reaches `updateAsr`. */
const INHERIT_GLOBAL = "__mode_inherit_global__";

/**
 * One mode, on one screen.
 *
 * Four things a reader came here to change — what to tell the model, which
 * model hears them, what comes out, and when the mode turns on by itself —
 * then one Advanced disclosure holding every remaining knob. The five-tab
 * editor this replaces spread the same twenty-nine rows across five surfaces,
 * so the answer to "what does this mode do" was never on screen at once.
 */
export const ModeEditor: React.FC<ModeEditorProps> = ({
  mode,
  savedMode,
  modeCount,
  models,
  onChange,
  onSave,
  saving,
  conflict,
  activationRules,
  websiteActivationRules,
  activationSupported,
  capturingActivation,
  onCaptureActivation,
  onRemoveActivation,
  onCaptureWebsiteActivation,
  onRemoveWebsiteActivation,
}) => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const [websiteMatchKind, setWebsiteMatchKind] =
    useState<WebsiteHostMatch>("exact");

  const updaters = useMemo(
    () => createModeDraftUpdaters(mode, onChange),
    [mode, onChange],
  );

  const {
    cloud,
    pendingConsent,
    consentError,
    accepting,
    acceptConsent,
    dismissConsent,
  } = useCloudSttEngineChoice(mode, updaters);
  const {
    requestedEngine,
    selectedProvider: selectedCloudProvider,
    controlsAvailable: cloudControlsAvailable,
  } = cloud;

  const vocabulary = useVocabularyRows(mode.asr.custom_words, (entries) =>
    updaters.updateAsr("custom_words", entries),
  );

  /* An empty mode model inherits the globally selected one, so it only leaves
   * the cloud route without a local fallback when nothing is selected there
   * either. Blocking Save on an empty field alone would be a false alarm. */
  const globalModelId = settings?.selected_model ?? "";
  const missingLocalFallbackModel =
    cloudControlsAvailable &&
    (mode.asr.local_fallback_enabled ?? true) &&
    !mode.asr.model_id.trim() &&
    !mode.asr.local_fallback_model_id?.trim() &&
    globalModelId.trim() === "";

  const nameMissing = mode.name.trim() === "";
  const blockingReason = nameMissing
    ? t("settings.modes.errors.empty_name")
    : vocabulary.incomplete
      ? t("settings.modes.recognition.vocabulary.incomplete")
      : missingLocalFallbackModel
        ? t("settings.modes.recognition.cloud.fallback.model.required")
        : null;

  const dirty = modeDraftIsDirty(mode, savedMode);

  /* An empty per-mode model is not "unset", it means "use whatever the app is
   * set to", which the backend resolves when it builds the plan. Naming the
   * model that will actually run is the difference between a default and a
   * gap. */
  const inheritGlobalLabel = useMemo(() => {
    const globalModel = models.find((model) => model.id === globalModelId);
    return globalModel
      ? t(
          "settings.modes.recognition.model.inheritGlobalNamed",
          "Default: the global model ({{name}})",
          { name: getTranslatedModelName(globalModel, t) },
        )
      : t(
          "settings.modes.recognition.model.inheritGlobal",
          "Default: the global model",
        );
  }, [globalModelId, models, t]);

  /* The same sentence one field over, for the rewrite provider. A mode with no
   * provider of its own is not "unset": it uses whatever the app is set to,
   * which `ModeLlmSettings::destination` resolves when it builds the plan.
   * Naming that provider is what would have shown, at a glance, that four
   * modes were pointed at a provider with no key while the app was pointed at
   * a local one that worked. */
  const llmDestination = modeLlmDestination(mode.llm, settings);
  const usesOfflineCleanup = llmDestination.providerId === "sona_local";
  /* The label names the *global* provider, not the current selection: it has
   * to read the same whether or not this mode has overridden it. */
  const globalProviderId = settings?.post_process_provider_id;
  const globalProviderLabel = settings?.post_process_providers?.find(
    (provider) => provider.id === globalProviderId,
  )?.label;
  const inheritProviderLabel = globalProviderLabel
    ? t(
        "settings.modes.writing.provider.inheritGlobalNamed",
        "Default: the global provider ({{name}})",
        { name: globalProviderLabel },
      )
    : t(
        "settings.modes.writing.provider.inheritGlobal",
        "Default: the global provider",
      );

  /* The list is a model rather than markup: a provider without a saved key
   * stays in it carrying the reason it cannot be picked, and the menu that
   * shows it only exists once a pointer has opened it. */
  const engineOptions = useMemo(
    () => modeEngineOptions(cloud.isConfigured, t),
    [cloud.isConfigured, t],
  );
  const selectedEngineLabel = engineOptions.find(
    (option) => option.value === requestedEngine,
  )?.label;

  const localModelOptions = useMemo(() => {
    const options = downloadedModelOptions(models);
    // A mode can name a model this install no longer has. Keep it selectable
    // so opening the editor never silently rewrites the saved choice.
    if (
      mode.asr.model_id !== "" &&
      !options.some((option) => option.value === mode.asr.model_id)
    ) {
      options.unshift({ value: mode.asr.model_id, label: mode.asr.model_id });
    }
    options.unshift({ value: INHERIT_GLOBAL, label: inheritGlobalLabel });
    return options;
  }, [inheritGlobalLabel, mode.asr.model_id, models]);

  const languageOptions = useMemo(
    () =>
      SELECTABLE_LANGUAGES.map((language) => ({
        value: language.value,
        label:
          language.value === "auto"
            ? t("settings.modes.recognition.language.auto")
            : language.label,
      })),
    [t],
  );

  const busyActivation = saving || capturingActivation;

  /* Apps and websites are the same promise — "when I am here, use this mode" —
   * so they are one list. A website rule carries its scope as the row's
   * detail, because that is the only thing about it a reader cannot see. */
  const activationItems: ActivationRuleItem[] = [];
  for (const rule of activationRules) {
    if (rule.mode_id !== mode.id) continue;
    activationItems.push({
      id: `app:${rule.app_id}`,
      target: rule.app_id,
      removeLabel: t("settings.modes.activation.removeTarget", {
        target: rule.app_id,
      }),
      onRemove: () => onRemoveActivation(rule.app_id),
    });
  }
  for (const rule of websiteActivationRules) {
    if (rule.mode_id !== mode.id) continue;
    activationItems.push({
      id: `site:${rule.host}:${rule.match_kind}`,
      target: rule.host,
      detail: t(
        `settings.modes.activation.website.scope.values.${rule.match_kind}`,
      ),
      removeLabel: t("settings.modes.activation.removeTarget", {
        target: rule.host,
      }),
      onRemove: () => onRemoveWebsiteActivation(rule.host, rule.match_kind),
    });
  }

  const websiteCaptureEnabled = settings?.context_url_capture_enabled ?? false;

  return (
    <div className="flex flex-col gap-6">
      {/* The name field is the editor's title: the mode's name already appears
       * on its row in the list, so a heading over the field that edits it
       * would be the third copy on one screen. */}
      <div className="flex items-center gap-3">
        <Input
          id="mode-name"
          value={mode.name}
          onChange={(event) => updaters.update("name", event.target.value)}
          aria-label={t("settings.modes.identity.name.label")}
          aria-invalid={nameMissing || undefined}
          placeholder={t("settings.modes.untitled", "Untitled mode")}
          maxLength={120}
          className="h-9 min-w-0 flex-1"
        />
        <Button
          size="sm"
          className="flex-none"
          onClick={onSave}
          /* The one sentence this header used to print on every render. It is
           * true of Save specifically, so it belongs to Save. */
          title={t("settings.modes.changesNextRun")}
          disabled={
            saving ||
            capturingActivation ||
            nameMissing ||
            vocabulary.incomplete ||
            missingLocalFallbackModel
          }
        >
          {saving ? t("settings.modes.saving") : t("settings.modes.save")}
        </Button>
      </div>

      {conflict ? (
        <Notice tone="danger">
          {t("settings.modes.errors.staleRevision")}
        </Notice>
      ) : null}
      {blockingReason ? (
        <Notice tone="danger">{blockingReason}</Notice>
      ) : dirty ? (
        <Notice>{t("settings.modes.unsavedChanges", "Unsaved changes")}</Notice>
      ) : null}

      <SettingsSection label={t("modesV2.sections.instructions")}>
        <SettingsRow
          label={t("modesV2.instructions.enabled.label")}
          controlId="mode-llm-enabled"
        >
          <Switch
            id="mode-llm-enabled"
            checked={mode.llm.enabled}
            onCheckedChange={(next) => updaters.updateLlm("enabled", next)}
          />
        </SettingsRow>
        <SettingsField
          label={t("modesV2.instructions.custom.label")}
          /* The one thing neither the label nor the box can show: written
           * instructions replace the output style further down, rather than
           * being added to it. */
          hint={t(
            usesOfflineCleanup
              ? "settings.postProcessing.offlineCleanup.lightOnly"
              : "modesV2.instructions.custom.hint",
          )}
          controlId="mode-instructions"
          disabled={!mode.llm.enabled || usesOfflineCleanup}
        >
          <Textarea
            id="mode-instructions"
            rows={4}
            value={mode.prompt.custom_prompt ?? ""}
            disabled={!mode.llm.enabled || usesOfflineCleanup}
            onChange={(event) =>
              updaters.updatePrompt(
                "custom_prompt",
                event.target.value === "" ? null : event.target.value,
              )
            }
            placeholder={t("modesV2.instructions.custom.placeholder")}
          />
        </SettingsField>
        {mode.llm.enabled ? null : (
          <div className="px-6 py-3">
            <Notice live={false}>{t("modesV2.instructions.disabled")}</Notice>
          </div>
        )}
      </SettingsSection>

      <SettingsSection label={t("modesV2.sections.model")}>
        <SettingsRow
          label={t("settings.modes.recognition.engine.label")}
          controlId="mode-engine"
        >
          <Select
            value={requestedEngine}
            onValueChange={(value) => {
              const next = REQUESTED_ENGINES.find(
                (candidate) => candidate === value,
              );
              if (next) cloud.selectEngine(next);
            }}
          >
            <SelectTrigger
              aria-label={t("settings.modes.recognition.engine.label")}
              id="mode-engine"
              size="sm"
              className="w-56"
            >
              {/* Radix resolves a trigger's text from its items, which only
               * mount client-side. Naming the selected option here keeps the
               * trigger legible in the server pass too. */}
              <SelectValue>{selectedEngineLabel}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              {engineOptions
                .filter((option) => !option.cloud)
                .map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              <SelectGroup>
                <SelectLabel>
                  {t("settings.modes.recognition.engine.cloud")}
                </SelectLabel>
                {engineOptions
                  .filter((option) => option.cloud)
                  .map((option) => (
                    <SelectItem
                      key={option.value}
                      value={option.value}
                      disabled={option.disabled}
                    >
                      {option.label}
                    </SelectItem>
                  ))}
              </SelectGroup>
            </SelectContent>
          </Select>
        </SettingsRow>

        {selectedCloudProvider ? null : (
          <SettingsRow
            label={t("settings.modes.recognition.model.label")}
            controlId="mode-model"
          >
            <Select
              value={
                mode.asr.model_id === "" ? INHERIT_GLOBAL : mode.asr.model_id
              }
              onValueChange={(value) =>
                updaters.updateAsr(
                  "model_id",
                  value === INHERIT_GLOBAL ? "" : value,
                )
              }
            >
              <SelectTrigger id="mode-model" size="sm" className="w-56">
                <SelectValue placeholder={inheritGlobalLabel} />
              </SelectTrigger>
              <SelectContent>
                {localModelOptions.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </SettingsRow>
        )}

        <SettingsRow
          label={t("settings.modes.recognition.language.label")}
          /* The cloud sentence is the one a reader cannot get from the label:
           * on a cloud engine the language is sent with the request rather
           * than handed to a local model. */
          hint={
            selectedCloudProvider
              ? t("settings.modes.recognition.language.cloudDescription")
              : undefined
          }
          controlId="mode-language"
        >
          <Select
            value={mode.asr.language}
            onValueChange={(language) =>
              updaters.updateAsr("language", language)
            }
          >
            <SelectTrigger id="mode-language" size="sm" className="w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {languageOptions.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingsRow>

        {selectedCloudProvider && !cloudControlsAvailable ? (
          <div className="px-6 py-3">
            <Notice tone="warning" live>
              {t("settings.modes.recognition.cloud.setupRequired", {
                provider: t(selectedCloudProvider.labelKey),
              })}
            </Notice>
          </div>
        ) : null}
      </SettingsSection>

      <SettingsSection label={t("modesV2.sections.output")}>
        <SettingsRow
          label={t("settings.modes.delivery.method.label")}
          controlId="mode-paste-method"
        >
          <Select
            value={mode.delivery.paste_method}
            onValueChange={(method) => {
              const next = PASTE_METHODS.find(
                (candidate) => candidate === method,
              );
              if (next) updaters.updateDelivery("paste_method", next);
            }}
          >
            <SelectTrigger id="mode-paste-method" size="sm" className="w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {PASTE_METHODS.map((method) => (
                <SelectItem key={method} value={method}>
                  {t(`settings.modes.delivery.method.values.${method}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingsRow>

        {/* The script is the method's own parameter, so it stays beside it
         * rather than behind the disclosure: a method that cannot run without
         * a path must not hide the path. */}
        {mode.delivery.paste_method === "external_script" ? (
          <SettingsField
            label={t("settings.modes.delivery.script.label")}
            controlId="mode-external-script"
          >
            <Input
              id="mode-external-script"
              value={mode.delivery.external_script_path ?? ""}
              onChange={(event) =>
                updaters.updateDelivery(
                  "external_script_path",
                  event.target.value || null,
                )
              }
            />
          </SettingsField>
        ) : null}

        <SettingsRow
          label={t("modesV2.output.style.label")}
          /* What a named style actually instructs stays hidden and
           * unexportable, which is the one fact the six names cannot show. */
          hint={t("settings.modes.writing.preset.description")}
          controlId="mode-output-style"
          disabled={!mode.llm.enabled || usesOfflineCleanup}
        >
          <Select
            value={mode.prompt.preset}
            disabled={!mode.llm.enabled || usesOfflineCleanup}
            onValueChange={(value) => {
              const next = PROMPT_PRESETS.find(
                (candidate) => candidate === value,
              );
              if (next) updaters.updatePrompt("preset", next);
            }}
          >
            <SelectTrigger id="mode-output-style" size="sm" className="w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {PROMPT_PRESETS.map((preset) => (
                <SelectItem key={preset} value={preset}>
                  {t(`settings.modes.writing.preset.values.${preset}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingsRow>
      </SettingsSection>

      <SettingsSection label={t("modesV2.activation.title")}>
        <SettingsField
          label={t("modesV2.activation.rules.label")}
          disabled={!activationSupported}
        >
          {!activationSupported ? (
            <Notice live={false}>
              {t("settings.modes.activation.unsupported")}
            </Notice>
          ) : (
            <ActivationRuleList
              label={t("modesV2.activation.rules.label")}
              items={activationItems}
              disabled={busyActivation}
              emptyTitle={t("modesV2.activation.rules.empty")}
              emptyDescription={t("modesV2.activation.rules.example")}
              removeText={t("settings.modes.activation.remove")}
              action={
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={busyActivation}
                    onClick={onCaptureActivation}
                  >
                    {capturingActivation
                      ? t("settings.modes.activation.capture.capturing")
                      : t("settings.modes.activation.capture.action")}
                  </Button>
                  {websiteCaptureEnabled ? (
                    <>
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        disabled={busyActivation}
                        onClick={() =>
                          onCaptureWebsiteActivation(websiteMatchKind)
                        }
                      >
                        {capturingActivation
                          ? t(
                              "settings.modes.activation.website.capture.capturing",
                            )
                          : t(
                              "settings.modes.activation.website.capture.action",
                            )}
                      </Button>
                      {/* The scope is this button's own argument — which host
                       * form the capture stores — so it sits beside it. */}
                      <Select
                        value={websiteMatchKind}
                        disabled={busyActivation}
                        onValueChange={(value) => {
                          const matchKind = WEBSITE_HOST_MATCHES.find(
                            (candidate) => candidate === value,
                          );
                          if (matchKind) setWebsiteMatchKind(matchKind);
                        }}
                      >
                        <SelectTrigger
                          id="mode-website-scope"
                          size="sm"
                          aria-label={t(
                            "settings.modes.activation.website.scope.label",
                          )}
                          className="w-48"
                        >
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {WEBSITE_HOST_MATCHES.map((matchKind) => (
                            <SelectItem key={matchKind} value={matchKind}>
                              {t(
                                `settings.modes.activation.website.scope.values.${matchKind}`,
                              )}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </>
                  ) : (
                    <Notice tone="warning" live={false}>
                      {t(
                        "settings.modes.errors.website_activation_consent_required",
                      )}
                    </Notice>
                  )}
                </div>
              }
            />
          )}
        </SettingsField>
      </SettingsSection>

      <ModeAdvanced
        mode={mode}
        updaters={updaters}
        models={models}
        modeCount={modeCount}
        cloud={cloud}
        vocabulary={vocabulary}
        missingFallbackModel={missingLocalFallbackModel}
        providers={settings?.post_process_providers ?? NO_PROVIDERS}
        contextCeiling={settings?.context_policy_ceiling ?? "none"}
        llmDestination={llmDestination}
        inheritProviderLabel={inheritProviderLabel}
      />

      <Dialog
        open={pendingConsent !== null}
        onOpenChange={(open) => {
          if (!open) dismissConsent();
        }}
      >
        <DialogContent
          showCloseButton={!accepting}
          /* Every exit is blocked while the accept is in flight — the close
             button, the backdrop, the Decline button (disabled below) and
             Escape. Escape is not cosmetic: onOpenChange(false) drops the
             pending ask, but acceptConsent captured it, so the in-flight call
             still records audio-transfer consent and switches the mode to the
             cloud provider after the user backed out — and a failure would
             write into state no open dialog renders. */
          onEscapeKeyDown={(event) => {
            if (accepting) event.preventDefault();
          }}
          onInteractOutside={(event) => {
            if (accepting) event.preventDefault();
          }}
        >
          <DialogHeader>
            <DialogTitle>
              {t("settings.modes.recognition.cloud.consent.title", {
                provider: pendingConsent ? t(pendingConsent.labelKey) : "",
              })}
            </DialogTitle>
            <DialogDescription>
              {t("settings.modes.recognition.cloud.consent.description", {
                provider: pendingConsent ? t(pendingConsent.labelKey) : "",
              })}
            </DialogDescription>
          </DialogHeader>

          <div className="flex flex-col gap-3 text-sm text-gray-900">
            <p>{t("settings.modes.recognition.cloud.consent.intro")}</p>
            <ul className="flex flex-col gap-2">
              <li>
                <p className="font-medium text-gray-1000">
                  {t(
                    "settings.modes.recognition.cloud.consent.audioTransfer.label",
                  )}
                </p>
                <p>
                  {t(
                    "settings.modes.recognition.cloud.consent.audioTransfer.description",
                    {
                      provider: pendingConsent
                        ? t(pendingConsent.labelKey)
                        : "",
                    },
                  )}
                </p>
              </li>
              <li>
                <p className="font-medium text-gray-1000">
                  {t("settings.modes.recognition.cloud.consent.privacy.label")}
                </p>
                <p>
                  {t(
                    "settings.modes.recognition.cloud.consent.privacy.description",
                  )}
                </p>
              </li>
              <li>
                <p className="font-medium text-gray-1000">
                  {t(
                    "settings.modes.recognition.cloud.consent.localFallback.label",
                  )}
                </p>
                <p>
                  {t(
                    "settings.modes.recognition.cloud.consent.localFallback.description",
                  )}
                </p>
              </li>
            </ul>
            {consentError ? (
              <Notice tone="danger" assertive>
                {t(
                  `settings.modes.recognition.cloud.consent.errors.${consentError}`,
                )}
              </Notice>
            ) : null}
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={dismissConsent}
              disabled={accepting}
            >
              {t("settings.modes.recognition.cloud.consent.decline")}
            </Button>
            <Button
              type="button"
              size="sm"
              onClick={acceptConsent}
              disabled={accepting}
            >
              {accepting
                ? t("settings.modes.recognition.cloud.consent.accepting")
                : t("settings.modes.recognition.cloud.consent.accept")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
};
