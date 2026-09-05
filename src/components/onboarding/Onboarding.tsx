import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { ModelInfo } from "@/bindings";
import { SettingsDisclosure } from "@/components/settings/rows";
import type { ModelCardStatus } from "./ModelCard";
import ModelCard from "./ModelCard";
import { isLegacySource } from "./modelSource";
import { SonaWordmark } from "../icons/SonaWordmark";
import { useModelStore } from "../../stores/modelStore";
import "./onboarding.css";

interface OnboardingProps {
  onModelSelected: () => void;
}

const Onboarding: React.FC<OnboardingProps> = ({ onModelSelected }) => {
  const { t } = useTranslation();
  const {
    models,
    downloadModel,
    selectModel,
    downloadingModels,
    verifyingModels,
    extractingModels,
    downloadProgress,
    cancelDownload,
  } = useModelStore();
  const [selectedModelId, setSelectedModelId] = useState<string | null>(null);
  const hasStartedSelection = useRef(false);

  const isBusy = selectedModelId !== null;

  /* One model on the page; every other model behind one summary.
   *
   * The catalog arrives rank-sorted (the backend sorts by rank_of, then
   * accuracy), so the first recommended download is the editorial pick — but a
   * model already on this Mac beats it, because choosing that one costs no
   * download at all. Legacy (.bin/ONNX) sources are never offered as downloads;
   * they still appear among the compatible models when they are already on
   * disk. */
  const { featured, onDisk, toDownload } = useMemo(() => {
    const downloaded = models.filter((model: ModelInfo) => model.is_downloaded);
    const downloadable = models.filter(
      (model: ModelInfo) => !model.is_downloaded && !isLegacySource(model),
    );
    const ranked = [
      ...downloadable.filter((model: ModelInfo) => model.is_recommended),
      ...downloadable.filter((model: ModelInfo) => !model.is_recommended),
    ];
    const pick = downloaded[0] ?? ranked[0] ?? null;
    return {
      featured: pick,
      onDisk: downloaded.filter((model: ModelInfo) => model !== pick),
      toDownload: ranked.filter((model: ModelInfo) => model !== pick),
    };
  }, [models]);

  const otherCount = onDisk.length + toDownload.length;

  // Watch for the selected model to finish downloading + verifying + extracting
  useEffect(() => {
    if (!selectedModelId) {
      hasStartedSelection.current = false;
      return;
    }

    const model = models.find((m) => m.id === selectedModelId);
    const stillDownloading = selectedModelId in downloadingModels;
    const stillVerifying = selectedModelId in verifyingModels;
    const stillExtracting = selectedModelId in extractingModels;

    if (
      model?.is_downloaded &&
      !stillDownloading &&
      !stillVerifying &&
      !stillExtracting &&
      !hasStartedSelection.current
    ) {
      hasStartedSelection.current = true;

      // Model is ready — select it and transition
      selectModel(selectedModelId).then((success) => {
        if (success) {
          onModelSelected();
        } else {
          toast.error(t("onboarding.errors.selectModel"));
          hasStartedSelection.current = false;
          setSelectedModelId(null);
        }
      });
    }
  }, [
    selectedModelId,
    models,
    downloadingModels,
    verifyingModels,
    extractingModels,
    selectModel,
    onModelSelected,
    t,
  ]);

  const handleDownloadModel = async (modelId: string) => {
    setSelectedModelId(modelId);

    // Error toast is handled centrally by the model-download-failed event listener
    // in modelStore — no toast here to avoid duplicates.
    const success = await downloadModel(modelId);
    if (!success) {
      setSelectedModelId(null);
    }
  };

  const handleCancelDownload = async (modelId: string) => {
    const success = await cancelDownload(modelId);
    if (success) {
      setSelectedModelId(null);
    }
  };

  const downloadStatus = (modelId: string): ModelCardStatus => {
    if (modelId in extractingModels) return "extracting";
    if (modelId in verifyingModels) return "verifying";
    if (modelId in downloadingModels) return "downloading";
    return "downloadable";
  };

  /* Every card except the one being worked on goes quiet while a model is
   * arriving: the reader has already chosen, and the row doing the work is the
   * only one with anything left to say. */
  const card = (model: ModelInfo) =>
    model.is_downloaded ? (
      <ModelCard
        key={model.id}
        model={model}
        status={selectedModelId === model.id ? "switching" : "available"}
        disabled={isBusy && selectedModelId !== model.id}
        onSelect={setSelectedModelId}
      />
    ) : (
      <ModelCard
        key={model.id}
        model={model}
        status={downloadStatus(model.id)}
        disabled={isBusy && selectedModelId !== model.id}
        onSelect={handleDownloadModel}
        onDownload={handleDownloadModel}
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
        <h1 className="ob-headline">{t("onboarding.headline")}</h1>
        <p className="ob-subhead">{t("onboarding.subtitle")}</p>

        <div className="ob-pick">
          {featured ? (
            card(featured)
          ) : (
            <p className="text-[13px] leading-5 text-gray-800">
              {t("modelSelector.noModelsAvailable")}
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
        </div>
      </div>
    </div>
  );
};

export default Onboarding;
