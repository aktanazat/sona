import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ChevronDown } from "lucide-react";
import { toast } from "sonner";
import { commands, type ModelInfo } from "@/bindings";
import { SettingsDisclosure } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/vg/command";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/vg/popover";
import { useSettings } from "@/hooks/useSettings";
import {
  getLanguageLabel,
  SELECTABLE_LANGUAGES,
  supportsLanguageCode,
} from "@/lib/constants/languages";
import type { ModelCardStatus } from "./ModelCard";
import ModelCard from "./ModelCard";
import OnboardingPractice from "./OnboardingPractice";
import { isLegacySource } from "./modelSource";
import { SonaWordmark } from "../icons/SonaWordmark";
import { useModelStore } from "../../stores/modelStore";
import "./onboarding.css";

interface OnboardingProps {
  onModelSelected: () => void;
}

const Onboarding: React.FC<OnboardingProps> = ({ onModelSelected }) => {
  const { t } = useTranslation();
  const { settings, isLoading, refreshSettings } = useSettings();
  const {
    models,
    loading,
    error: modelError,
    loadModels,
    downloadModel,
    selectModel,
    downloadingModels,
    verifyingModels,
    extractingModels,
    downloadProgress,
    cancelDownload,
  } = useModelStore();
  const [step, setStep] = useState<"languages" | "model" | "practice">(
    "languages",
  );
  const [languages, setLanguages] = useState<string[] | null>(null);
  const [languagePickerOpen, setLanguagePickerOpen] = useState(false);
  const [savingLanguages, setSavingLanguages] = useState(false);
  const [languageError, setLanguageError] = useState(false);
  const [selectedModelId, setSelectedModelId] = useState<string | null>(null);
  const selection = useRef<{ id: string; started: boolean } | null>(null);
  const mounted = useRef(true);
  const saving = useRef(false);

  const chosenLanguages =
    languages ??
    settings?.dictation_languages ??
    (settings?.selected_language && settings.selected_language !== "auto"
      ? [settings.selected_language]
      : []);
  const languageNames = chosenLanguages.length
    ? chosenLanguages.map((code) => getLanguageLabel(code) ?? code).join(", ")
    : t("onboarding.languages.auto");
  const isBusy = selectedModelId !== null;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      selection.current = null;
    };
  }, []);

  const { featured, onDisk, toDownload } = useMemo(() => {
    const compatible = models.filter((model) =>
      chosenLanguages.length
        ? chosenLanguages.every((language) =>
            supportsLanguageCode(model.supported_languages, language),
          )
        : model.supports_language_detection &&
          model.supported_languages.length > 1,
    );
    const downloaded = compatible.filter((model) => model.is_downloaded);
    const downloadable = compatible.filter(
      (model) => !model.is_downloaded && !isLegacySource(model),
    );
    const ranked = [
      ...downloadable.filter((model) => model.is_recommended),
      ...downloadable.filter((model) => !model.is_recommended),
    ];
    const pick = downloaded[0] ?? ranked[0] ?? null;
    return {
      featured: pick,
      onDisk: downloaded.filter((model) => model !== pick),
      toDownload: ranked.filter((model) => model !== pick),
    };
  }, [models, chosenLanguages]);

  const otherCount = onDisk.length + toDownload.length;

  useEffect(() => {
    const choice = selection.current;
    if (!choice || choice.started || step !== "model") return;
    const model = models.find((candidate) => candidate.id === choice.id);
    const working =
      choice.id in downloadingModels ||
      choice.id in verifyingModels ||
      choice.id in extractingModels;
    if (working) return;
    if (!model?.is_downloaded) {
      if (modelError) {
        selection.current = null;
        setSelectedModelId(null);
      }
      return;
    }
    choice.started = true;
    void selectModel(choice.id).then((success) => {
      if (!mounted.current || selection.current !== choice) return;
      selection.current = null;
      setSelectedModelId(null);
      if (success) setStep("practice");
      else toast.error(t("onboarding.errors.selectModel"));
    });
  }, [
    selectedModelId,
    step,
    models,
    modelError,
    downloadingModels,
    verifyingModels,
    extractingModels,
    selectModel,
    t,
  ]);

  const continueToModels = async () => {
    if (saving.current || isLoading) return;
    saving.current = true;
    setSavingLanguages(true);
    setLanguageError(false);
    try {
      const result =
        await commands.changeDictationLanguagesSetting(chosenLanguages);
      if (!mounted.current) return;
      if (result.status === "error") throw result.error;
      setLanguages(chosenLanguages);
      await refreshSettings();
      if (mounted.current) setStep("model");
    } catch {
      if (mounted.current) setLanguageError(true);
    } finally {
      saving.current = false;
      if (mounted.current) setSavingLanguages(false);
    }
  };

  const chooseModel = async (modelId: string) => {
    if (selection.current) return;
    const choice = { id: modelId, started: false };
    selection.current = choice;
    setSelectedModelId(modelId);
    if (models.find((model) => model.id === modelId)?.is_downloaded) return;
    const success = await downloadModel(modelId);
    if (!success && mounted.current && selection.current === choice) {
      selection.current = null;
      setSelectedModelId(null);
    }
  };

  const handleCancelDownload = async (modelId: string) => {
    selection.current = null;
    const success = await cancelDownload(modelId);
    if (!mounted.current) return;
    setSelectedModelId(null);
    if (!success) toast.error(t("onboarding.errors.cancelDownload"));
  };

  const downloadStatus = (modelId: string): ModelCardStatus => {
    if (modelId in extractingModels) return "extracting";
    if (modelId in verifyingModels) return "verifying";
    if (modelId in downloadingModels) return "downloading";
    return "downloadable";
  };

  const card = (model: ModelInfo) => (
    <ModelCard
      key={model.id}
      model={model}
      status={
        model.is_downloaded
          ? selectedModelId === model.id
            ? "switching"
            : "available"
          : downloadStatus(model.id)
      }
      disabled={isBusy && selectedModelId !== model.id}
      onSelect={chooseModel}
      onDownload={chooseModel}
      onCancel={handleCancelDownload}
      downloadProgress={downloadProgress[model.id]?.percentage}
    />
  );

  return (
    <div className="onboarding-shell ob-stage">
      <div className="ob-column">
        <div className="ob-brand">
          <SonaWordmark className="text-[14px]" />
        </div>
        {step === "languages" ? (
          <>
            <h1 className="ob-headline">{t("onboarding.languages.title")}</h1>
            <div className="ob-pick">
              <Popover
                open={languagePickerOpen}
                onOpenChange={setLanguagePickerOpen}
              >
                <PopoverTrigger asChild>
                  <Button
                    variant="outline"
                    role="combobox"
                    aria-expanded={languagePickerOpen}
                    aria-label={t("onboarding.languages.selector", {
                      languages: languageNames,
                    })}
                    disabled={savingLanguages || isLoading}
                    className="ob-language-picker"
                  >
                    <span>{languageNames}</span>
                    <ChevronDown aria-hidden="true" />
                  </Button>
                </PopoverTrigger>
                <PopoverContent align="start" className="w-72 p-0">
                  <Command>
                    <CommandInput
                      placeholder={t(
                        "settings.general.language.searchPlaceholder",
                      )}
                    />
                    <CommandList>
                      <CommandEmpty>
                        {t("settings.general.language.noResults")}
                      </CommandEmpty>
                      {SELECTABLE_LANGUAGES.map((language) => {
                        const checked =
                          language.value === "auto"
                            ? chosenLanguages.length === 0
                            : chosenLanguages.includes(language.value);
                        const label =
                          language.value === "auto"
                            ? t("onboarding.languages.auto")
                            : language.label;
                        return (
                          <CommandItem
                            key={language.value}
                            value={label}
                            aria-label={
                              checked
                                ? t("onboarding.languages.selected", {
                                    language: label,
                                  })
                                : label
                            }
                            onSelect={() =>
                              setLanguages(
                                language.value === "auto"
                                  ? []
                                  : checked
                                    ? chosenLanguages.filter(
                                        (code) => code !== language.value,
                                      )
                                    : [...chosenLanguages, language.value],
                              )
                            }
                          >
                            <Check
                              aria-hidden="true"
                              className={checked ? "opacity-100" : "opacity-0"}
                            />
                            {label}
                          </CommandItem>
                        );
                      })}
                    </CommandList>
                  </Command>
                </PopoverContent>
              </Popover>
              {languageError && (
                <p className="ob-error" role="alert">
                  {t("onboarding.errors.languages")}
                </p>
              )}
              <div className="ob-actions">
                <Button
                  disabled={savingLanguages || isLoading}
                  onClick={() => void continueToModels()}
                >
                  {t(savingLanguages ? "common.saving" : "onboarding.continue")}
                </Button>
              </div>
            </div>
          </>
        ) : step === "practice" ? (
          <OnboardingPractice
            onComplete={onModelSelected}
            onBack={() => setStep("model")}
          />
        ) : (
          <>
            <h1 className="ob-headline">{t("onboarding.headline")}</h1>
            <p className="ob-subhead">{languageNames}</p>
            <div className="ob-pick">
              {featured ? (
                card(featured)
              ) : (
                <p className="ob-practice-status" role="status">
                  {t(
                    loading
                      ? "common.loading"
                      : "onboarding.languages.noModels",
                  )}
                </p>
              )}
              {otherCount > 0 && (
                <SettingsDisclosure
                  label={t("onboarding.otherModels")}
                  fact={t("onboarding.moreModels", { total: otherCount })}
                >
                  {onDisk.length > 0 && (
                    <section className="ob-group">
                      <h2 className="ob-group-label">
                        {t("onboarding.existingModelsTitle")}
                      </h2>
                      {onDisk.map(card)}
                    </section>
                  )}
                  {toDownload.length > 0 && (
                    <section className="ob-group">
                      <h2 className="ob-group-label">
                        {t("onboarding.downloadModelsTitle")}
                      </h2>
                      {toDownload.map(card)}
                    </section>
                  )}
                </SettingsDisclosure>
              )}
              {modelError && !featured && (
                <>
                  <p className="ob-error" role="alert">
                    {modelError}
                  </p>
                  <Button variant="outline" onClick={() => void loadModels()}>
                    {t("common.retry")}
                  </Button>
                </>
              )}
              <div className="ob-actions">
                <Button
                  variant="ghost"
                  disabled={isBusy}
                  onClick={() => setStep("languages")}
                >
                  {t("onboarding.back")}
                </Button>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
};

export default Onboarding;
