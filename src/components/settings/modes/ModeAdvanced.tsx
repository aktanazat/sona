import React, { useMemo } from "react";
import { Plus, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type {
  ContextPolicy,
  ModelInfo,
  PostProcessProvider,
  Tone,
  VocabularyEntry,
} from "@/bindings";
import { Button } from "@/components/vg/button";
import { Input } from "@/components/vg/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { Slider } from "@/components/vg/slider";
import { Switch } from "@/components/vg/switch";
import { Textarea } from "@/components/vg/textarea";
import {
  Microlabel,
  Notice,
  SettingsDisclosure,
  SettingsField,
  SettingsRow,
  SettingsSurface,
} from "@/components/settings/rows";
import { useOsType } from "@/hooks/useOsType";
import { ModelField } from "../PostProcessingSettingsApi/ModelField";
import { usePostProcessModelCatalog } from "../PostProcessingSettingsApi/usePostProcessModelCatalog";
import { ShortcutInput } from "../ShortcutInput";
import { SegmentedRadioGroup, type SegmentedOption } from "./ModeControls";
import {
  AUTO_SUBMIT_KEYS,
  CLIPBOARD_HANDLING,
  CONTEXT_POLICIES,
  DEFAULT_FALLBACK_MODEL_OPTION,
  INHERIT_GLOBAL_PROVIDER,
  TONES,
  downloadedModelOptions,
  hasHigherPolicy,
  modeBindingId,
  type ModeCloudState,
  type ModeLlmDestinationView,
  type ModePanelProps,
} from "./modeModel";

export interface ModeVocabularyEditor {
  /** Stable key per entry object, so editing one row never remounts another. */
  rowKey: (entry: VocabularyEntry) => string;
  setField: (index: number, field: "spoken" | "written", value: string) => void;
  add: () => void;
  remove: (index: number) => void;
  incomplete: boolean;
}

export interface ModeAdvancedProps extends ModePanelProps {
  models: ModelInfo[];
  modeCount: number;
  cloud: ModeCloudState;
  vocabulary: ModeVocabularyEditor;
  /** Cloud fallback is on but neither this mode nor the fallback names a model. */
  missingFallbackModel: boolean;
  providers: readonly PostProcessProvider[];
  /** The most revealing context level Privacy currently permits. */
  contextCeiling: ContextPolicy;
  /** Which provider and model this draft's rewrite will actually use. */
  llmDestination: ModeLlmDestinationView;
  /** The inherit option's label, naming the global provider it resolves to. */
  inheritProviderLabel: string;
}

/* Shared by the vocabulary column header and every vocabulary row, so the two
 * fields line up and the trailing column stays wide enough for remove. No
 * horizontal padding: this list sits inside a field that already owns it. */
const VOCABULARY_GRID =
  "grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-2 py-2";

/* The two delays bracket the paste keystroke and fail in opposite directions
 * — one loses the transcript, the other pastes your previous clipboard — so
 * the field names both sides rather than either symptom. */
const PASTE_DELAYS = [
  { key: "paste_delay_ms", side: "settings.modes.delivery.delay.before" },
  {
    key: "paste_delay_after_ms",
    side: "settings.modes.delivery.delay.after",
  },
] as const;

/**
 * Everything about a mode that a good default already answers.
 *
 * One disclosure, one flat list of rows. The kit has no collapsible, so this
 * is `<details>` — keyboard and screen-reader behaviour for free, and closed
 * on arrival so the four decisions above it stay the screen. There is no
 * second disclosure inside: a knob is either here or it is gone.
 */
export const ModeAdvanced: React.FC<ModeAdvancedProps> = ({
  mode,
  updaters,
  models,
  modeCount,
  cloud,
  vocabulary,
  missingFallbackModel,
  providers,
  contextCeiling,
  llmDestination,
  inheritProviderLabel,
}) => {
  const { t } = useTranslation();
  const osType = useOsType();
  const { update, updateAsr, updateDelivery, updateLlm } = updaters;
  const llmEnabled = mode.llm.enabled;
  const usesOfflineCleanup = llmDestination.providerId === "sona_local";

  const toneOptions: SegmentedOption<Tone>[] = TONES.map((tone) => ({
    value: tone,
    label: t(`settings.modes.writing.tone.values.${tone}`),
  }));

  const providerOptions: { value: string; label: string }[] = providers.map(
    (provider) => ({ value: provider.id, label: provider.label }),
  );
  /* The provider the run will use, named the way the reader sees it named
   * elsewhere. Undefined means this install does not carry it. */
  const effectiveProviderLabel = providers.find(
    (provider) => provider.id === llmDestination.providerId,
  )?.label;
  /* A mode can name a provider this install does not have. Keep it selectable
   * so saving the mode never silently rewrites the choice, and explain it. */
  if (!llmDestination.inherited && !effectiveProviderLabel) {
    providerOptions.unshift({
      value: llmDestination.providerId,
      label: llmDestination.providerId,
    });
  }
  /* Inherit leads the list because it is the default, and its label names the
   * provider it resolves to. That naming is the whole control: four modes on
   * this machine said `openai` with no key while the app was set to a local
   * provider that worked, and this row held both facts and stated neither. */
  providerOptions.unshift({
    value: INHERIT_GLOBAL_PROVIDER,
    label: inheritProviderLabel,
  });
  const selectedProviderValue = mode.llm.provider_id ?? INHERIT_GLOBAL_PROVIDER;
  /* Radix reads the trigger's text out of the mounted items, which only exist
   * once the list has been opened in a browser. Name the selected label here
   * so the row states its own value in the server pass too. */
  const selectedProviderLabel = providerOptions.find(
    (option) => option.value === selectedProviderValue,
  )?.label;
  const explicitProvider = !llmDestination.inherited
    ? providers.find((provider) => provider.id === llmDestination.providerId)
    : undefined;
  const isFixedExplicitProvider =
    explicitProvider?.id === "apple_intelligence" ||
    explicitProvider?.id === "sona_local";
  const explicitModelCatalog = usePostProcessModelCatalog({
    autoDiscover: false,
    baseUrl: explicitProvider?.base_url ?? "",
    enabled: explicitProvider !== undefined && !isFixedExplicitProvider,
    providerId: llmDestination.providerId,
    savedModelId: llmDestination.modelId,
  });

  const ceilingLabel = t(
    `settings.modes.context.policy.values.${contextCeiling}`,
  );
  const blockedByCeiling = t("settings.modes.context.policy.blockedByCeiling", {
    ceiling: ceilingLabel,
  });
  const policyOptions: SegmentedOption<ContextPolicy>[] = CONTEXT_POLICIES.map(
    (policy) => {
      const blocked = hasHigherPolicy(policy, contextCeiling);
      return {
        value: policy,
        label: t(`settings.modes.context.policy.values.${policy}`),
        disabled: blocked,
        reason: blocked ? blockedByCeiling : undefined,
      };
    },
  );
  const selectionAboveCeiling =
    mode.context_policy !== "none" &&
    hasHigherPolicy(mode.context_policy, contextCeiling);

  const fallbackModelOptions = useMemo(() => {
    const options = downloadedModelOptions(models);
    const selected = mode.asr.local_fallback_model_id;
    if (selected && !options.some((option) => option.value === selected)) {
      options.unshift({ value: selected, label: selected });
    }
    options.unshift({
      value: DEFAULT_FALLBACK_MODEL_OPTION,
      label: t("settings.modes.recognition.cloud.fallback.useModeModel"),
    });
    return options;
  }, [mode.asr.local_fallback_model_id, models, t]);

  const localFallbackEnabled = mode.asr.local_fallback_enabled ?? true;
  const cloudRoute = cloud.selectedProvider && cloud.controlsAvailable;

  return (
    <SettingsSurface>
      <SettingsDisclosure
        label={t("modesV2.advanced.title")}
        fact={t("modesV2.advanced.summary")}
      >
        <>
          {/* Past the ninth mode there is no numbered switch chord left to
           * assign, so a new mode arrives unbound. That is the one thing this
           * list cannot show, and only past nine modes. */}
          {modeCount > 9 ? (
            <div className="px-6 py-3">
              <Notice live={false}>
                {t("settings.modes.shortcuts.manyModes")}
              </Notice>
            </div>
          ) : null}
          <ShortcutInput shortcutId={modeBindingId(mode.id, "transcribe")} />
          <ShortcutInput shortcutId={modeBindingId(mode.id, "switch")} />

          <SettingsField
            disabled={!llmEnabled || usesOfflineCleanup}
            label={t("settings.modes.writing.tone.label")}
          >
            <SegmentedRadioGroup
              name="mode-tone"
              legend={t("settings.modes.writing.tone.label")}
              value={mode.tone}
              options={toneOptions}
              onChange={(tone) => update("tone", tone)}
              disabled={!llmEnabled || usesOfflineCleanup}
            />
          </SettingsField>

          <SettingsRow
            disabled={!llmEnabled}
            label={t("settings.modes.writing.provider.label")}
            controlId="mode-llm-provider"
          >
            <div className="flex flex-col items-end gap-1">
              <Select
                value={selectedProviderValue}
                onValueChange={(value) =>
                  updateLlm(
                    "provider_id",
                    value === INHERIT_GLOBAL_PROVIDER ? null : value,
                  )
                }
                disabled={!llmEnabled || providerOptions.length === 0}
              >
                <SelectTrigger
                  id="mode-llm-provider"
                  size="sm"
                  className="w-56"
                >
                  <SelectValue
                    placeholder={t("settings.modes.writing.provider.empty")}
                  >
                    {selectedProviderLabel}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {providerOptions.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {option.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {providers.length === 0 ? (
                <Notice tone="warning" live={false}>
                  {t("settings.modes.writing.provider.noneConfigured")}
                </Notice>
              ) : effectiveProviderLabel ? null : (
                <Notice tone="warning" live={false}>
                  {t("settings.modes.writing.provider.unknownSelected")}
                </Notice>
              )}
            </div>
          </SettingsRow>

          <SettingsField
            disabled={!llmEnabled}
            label={t("settings.modes.writing.model.label")}
            controlId="mode-llm-model"
          >
            {llmDestination.inherited ? (
              <Input
                id="mode-llm-model"
                value={llmDestination.modelId}
                disabled
                className="w-full"
              />
            ) : !explicitProvider ? (
              <Input
                id="mode-llm-model"
                value={llmDestination.modelId}
                onChange={(event) => updateLlm("model_id", event.target.value)}
                disabled={!llmEnabled}
                className="w-full"
              />
            ) : isFixedExplicitProvider ? (
              <span className="text-sm text-gray-900">
                {llmDestination.modelId}
              </span>
            ) : (
              <ModelField
                id="mode-llm-model"
                value={llmDestination.modelId}
                options={explicitModelCatalog.modelOptions}
                allowCustom={explicitModelCatalog.allowsManualModelId}
                disabled={!llmEnabled}
                isLoading={explicitModelCatalog.isLoading}
                statusKeys={explicitModelCatalog.statusKeys}
                onOpen={() => {
                  void explicitModelCatalog.discoverIfNeeded();
                }}
                onSelect={(modelId) => updateLlm("model_id", modelId)}
                onCreate={(modelId) => updateLlm("model_id", modelId)}
                onRefresh={() => void explicitModelCatalog.discover()}
              />
            )}
          </SettingsField>

          <SettingsRow
            disabled={!llmEnabled || usesOfflineCleanup}
            label={t("settings.modes.writing.spokenInstructions.label")}
            /* The cue itself, and that saying it is not the same as typing
             * it — neither is inferable from the row's name. */
            hint={t(
              usesOfflineCleanup
                ? "settings.postProcessing.offlineCleanup.instructionsUnavailable"
                : "settings.modes.writing.spokenInstructions.description",
            )}
            controlId="mode-spoken-instructions"
          >
            <Switch
              id="mode-spoken-instructions"
              checked={mode.llm.spoken_instructions ?? false}
              disabled={!llmEnabled || usesOfflineCleanup}
              onCheckedChange={(enabled) =>
                updateLlm("spoken_instructions", enabled)
              }
            />
          </SettingsRow>

          <SettingsField
            label={t("settings.modes.context.policy.label")}
            /* Not inferable from four level names: the ceiling outranks
             * whatever this mode asks for. */
            hint={t("settings.modes.context.policy.description")}
          >
            <SegmentedRadioGroup
              layout="grid"
              name="mode-context-policy"
              legend={t("settings.modes.context.policy.label")}
              value={mode.context_policy}
              options={policyOptions}
              onChange={(policy) => update("context_policy", policy)}
            />
            {selectionAboveCeiling ? (
              <Notice tone="warning" live={false} className="mt-2">
                {t("settings.modes.context.policy.limitedByPrivacy")}
              </Notice>
            ) : policyOptions.some((option) => option.disabled) ? (
              <Notice live={false} className="mt-2">
                {`${blockedByCeiling} ${t(
                  "settings.modes.context.policy.raiseCeiling",
                )}`}
              </Notice>
            ) : null}
          </SettingsField>

          <SettingsRow
            label={t("settings.modes.recognition.translate.label")}
            controlId="mode-translate"
          >
            <Switch
              id="mode-translate"
              checked={mode.asr.translate_to_english}
              onCheckedChange={(enabled) =>
                updateAsr("translate_to_english", enabled)
              }
            />
          </SettingsRow>

          <SettingsRow
            label={t("settings.modes.recognition.literalPunctuation.label")}
            /* Which direction the conversion runs, and that it happens before
             * vocabulary corrections. */
            hint={t(
              "settings.modes.recognition.literalPunctuation.description",
            )}
            controlId="mode-literal-punctuation"
          >
            <Switch
              id="mode-literal-punctuation"
              checked={mode.asr.literal_punctuation ?? false}
              onCheckedChange={(enabled) =>
                updateAsr("literal_punctuation", enabled)
              }
            />
          </SettingsRow>

          <SettingsRow
            label={t("settings.modes.recognition.fillerRemoval.label")}
            controlId="mode-filler-removal"
          >
            <Switch
              id="mode-filler-removal"
              checked={mode.asr.filler_word_removal_enabled}
              onCheckedChange={(enabled) =>
                updateAsr("filler_word_removal_enabled", enabled)
              }
            />
          </SettingsRow>

          <SettingsRow
            label={t("settings.modes.recognition.vad.label")}
            controlId="mode-vad"
          >
            <Switch
              id="mode-vad"
              checked={mode.asr.vad_enabled}
              onCheckedChange={(enabled) => updateAsr("vad_enabled", enabled)}
            />
          </SettingsRow>

          <SettingsRow
            label={t("settings.modes.delivery.clipboard.label")}
            controlId="mode-clipboard-handling"
          >
            <Select
              value={mode.delivery.clipboard_handling}
              onValueChange={(handling) => {
                const next = CLIPBOARD_HANDLING.find(
                  (candidate) => candidate === handling,
                );
                if (next) updateDelivery("clipboard_handling", next);
              }}
            >
              <SelectTrigger
                id="mode-clipboard-handling"
                size="sm"
                className="w-56"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {CLIPBOARD_HANDLING.map((handling) => (
                  <SelectItem key={handling} value={handling}>
                    {t(`settings.modes.delivery.clipboard.values.${handling}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </SettingsRow>

          <SettingsRow
            label={t("settings.modes.delivery.autoSubmit.label")}
            controlId="mode-auto-submit"
          >
            <Switch
              id="mode-auto-submit"
              checked={mode.delivery.auto_submit}
              onCheckedChange={(enabled) =>
                updateDelivery("auto_submit", enabled)
              }
            />
          </SettingsRow>

          <SettingsRow
            label={t("settings.modes.delivery.autoSubmitKey.label")}
            controlId="mode-auto-submit-key"
            disabled={!mode.delivery.auto_submit}
          >
            <Select
              value={mode.delivery.auto_submit_key}
              disabled={!mode.delivery.auto_submit}
              onValueChange={(key) => {
                const next = AUTO_SUBMIT_KEYS.find(
                  (candidate) => candidate === key,
                );
                if (next) updateDelivery("auto_submit_key", next);
              }}
            >
              <SelectTrigger
                id="mode-auto-submit-key"
                size="sm"
                className="w-56"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {AUTO_SUBMIT_KEYS.map((key) => (
                  <SelectItem key={key} value={key}>
                    {t(`settings.modes.delivery.autoSubmitKey.values.${key}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </SettingsRow>

          <SettingsRow
            label={t("settings.modes.delivery.trailingSpace.label")}
            controlId="mode-trailing-space"
          >
            <Switch
              id="mode-trailing-space"
              checked={mode.delivery.append_trailing_space}
              onCheckedChange={(enabled) =>
                updateDelivery("append_trailing_space", enabled)
              }
            />
          </SettingsRow>

          {/* One field, two sliders: the delays bracket the paste keystroke
           * and are read together, which is what the copy written for them
           * already assumes. */}
          <SettingsField
            label={t("settings.modes.delivery.delay.label")}
            hint={t("settings.modes.delivery.delay.description")}
          >
            {PASTE_DELAYS.map(({ key, side }) => (
              <div key={key} className="flex items-center gap-3">
                <Microlabel className="w-12 shrink-0">{t(side)}</Microlabel>
                <Slider
                  aria-label={`${t("settings.modes.delivery.delay.label")} ${t(side)}`}
                  className="w-40"
                  value={[mode.delivery[key]]}
                  min={10}
                  max={500}
                  step={10}
                  onValueChange={([next]) => updateDelivery(key, next)}
                />
                <Microlabel className="tabular-nums">
                  {`${mode.delivery[key]}ms`}
                </Microlabel>
              </div>
            ))}
          </SettingsField>

          {/* The receipt-sequenced path is implemented for macOS and Windows.
           * The field is still per-mode everywhere; only the control is
           * platform-gated, exactly as the row it replaces was. */}
          {osType === "macos" || osType === "windows" ? (
            <SettingsRow
              label={t("settings.modes.delivery.reliablePaste.label")}
              hint={t("settings.modes.delivery.reliablePaste.description")}
              controlId="mode-reliable-paste"
            >
              <Switch
                id="mode-reliable-paste"
                checked={mode.delivery.reliable_paste}
                onCheckedChange={(enabled) =>
                  updateDelivery("reliable_paste", enabled)
                }
              />
            </SettingsRow>
          ) : null}

          {cloudRoute ? (
            <SettingsRow
              label={t("settings.modes.recognition.cloud.fallback.label")}
              /* Which failure hands the run back to a local model, and that
               * the captured audio is what gets re-transcribed. */
              hint={t("settings.modes.recognition.cloud.fallback.description")}
              controlId="mode-local-fallback"
            >
              <Switch
                id="mode-local-fallback"
                checked={localFallbackEnabled}
                onCheckedChange={(enabled) =>
                  updateAsr("local_fallback_enabled", enabled)
                }
              />
            </SettingsRow>
          ) : null}

          {cloudRoute && localFallbackEnabled ? (
            <SettingsField
              label={t("settings.modes.recognition.cloud.fallback.model.label")}
              controlId="mode-fallback-model"
            >
              <div className="flex flex-col gap-1.5">
                <Select
                  value={
                    mode.asr.local_fallback_model_id ??
                    DEFAULT_FALLBACK_MODEL_OPTION
                  }
                  onValueChange={(modelId) =>
                    updateAsr(
                      "local_fallback_model_id",
                      modelId === DEFAULT_FALLBACK_MODEL_OPTION
                        ? null
                        : modelId,
                    )
                  }
                >
                  <SelectTrigger
                    id="mode-fallback-model"
                    size="sm"
                    className="w-full"
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {fallbackModelOptions.map((option) => (
                      <SelectItem key={option.value} value={option.value}>
                        {option.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {missingFallbackModel ? (
                  <Notice tone="danger" live>
                    {t(
                      "settings.modes.recognition.cloud.fallback.model.required",
                    )}
                  </Notice>
                ) : null}
              </div>
            </SettingsField>
          ) : null}

          {cloudRoute ? (
            <SettingsField
              label={t("settings.modes.recognition.cloud.keyterms.label")}
              /* One term per line is the parsing contract, and the terms leave
               * the device with the request. */
              hint={t("settings.modes.recognition.cloud.keyterms.description")}
              controlId="mode-cloud-keyterms"
            >
              <Textarea
                id="mode-cloud-keyterms"
                value={mode.asr.cloud_keyterms?.join("\n") ?? ""}
                onChange={(event) => {
                  const keyterms: string[] = [];
                  for (const line of event.target.value.split("\n")) {
                    const keyterm = line.trim();
                    if (keyterm.length > 0) keyterms.push(keyterm);
                  }
                  updateAsr("cloud_keyterms", keyterms);
                }}
                placeholder={t(
                  "settings.modes.recognition.cloud.keyterms.placeholder",
                )}
                rows={3}
              />
            </SettingsField>
          ) : null}

          <SettingsField
            label={t("settings.modes.recognition.vocabulary.label")}
          >
            {/* The action sits above the rows, as it does in every other rule
             * list here: a field's trailing slot is for a measured value. */}
            <div className="mb-2 flex items-center justify-start">
              <Button variant="outline" size="sm" onClick={vocabulary.add}>
                <Plus aria-hidden="true" className="h-4 w-4" />
                {t("settings.modes.recognition.vocabulary.add")}
              </Button>
            </div>
            {mode.asr.custom_words.length === 0 ? (
              <Notice live={false}>
                {t("settings.modes.recognition.vocabulary.empty")}
              </Notice>
            ) : (
              <div>
                {/* Hidden from assistive tech: every field below carries its
                 * own label, so this is a visual alignment cue. */}
                <div aria-hidden="true" className={VOCABULARY_GRID}>
                  <Microlabel className="truncate">
                    {t("settings.modes.recognition.vocabulary.spoken")}
                  </Microlabel>
                  <Microlabel className="truncate">
                    {t("settings.modes.recognition.vocabulary.written")}
                  </Microlabel>
                </div>
                <ul
                  // Tailwind's reset drops the marker, which also drops list
                  // semantics in WebKit. The explicit role puts them back.
                  role="list"
                  aria-label={t("settings.modes.recognition.vocabulary.label")}
                  className="divide-y divide-gray-alpha-400"
                >
                  {mode.asr.custom_words.map((entry, index) => (
                    <li
                      key={vocabulary.rowKey(entry)}
                      className={VOCABULARY_GRID}
                    >
                      <Input
                        value={entry.spoken}
                        onChange={(event) =>
                          vocabulary.setField(
                            index,
                            "spoken",
                            event.target.value,
                          )
                        }
                        placeholder={t(
                          "settings.modes.recognition.vocabulary.spokenPlaceholder",
                        )}
                        aria-label={t(
                          "settings.modes.recognition.vocabulary.spoken",
                        )}
                        aria-invalid={entry.spoken.trim() === "" || undefined}
                      />
                      <Input
                        value={entry.written}
                        onChange={(event) =>
                          vocabulary.setField(
                            index,
                            "written",
                            event.target.value,
                          )
                        }
                        placeholder={t(
                          "settings.modes.recognition.vocabulary.writtenPlaceholder",
                        )}
                        aria-label={t(
                          "settings.modes.recognition.vocabulary.written",
                        )}
                        aria-invalid={entry.written.trim() === "" || undefined}
                      />
                      <Button
                        variant="ghost"
                        size="icon"
                        className="justify-self-end text-red-900 hover:text-red-900"
                        aria-label={t(
                          "settings.modes.recognition.vocabulary.remove",
                          { spoken: entry.spoken },
                        )}
                        onClick={() => vocabulary.remove(index)}
                      >
                        <Trash2 aria-hidden="true" className="h-4 w-4" />
                      </Button>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {vocabulary.incomplete ? (
              <Notice tone="danger" live className="mt-2">
                {t("settings.modes.recognition.vocabulary.incomplete")}
              </Notice>
            ) : null}
          </SettingsField>
        </>
      </SettingsDisclosure>
    </SettingsSurface>
  );
};
