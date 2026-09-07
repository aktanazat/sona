import React, { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ask } from "@tauri-apps/plugin-dialog";
import type { ModelInfo } from "@/bindings";
import { Button } from "@/components/vg/button";
import { Skeleton } from "@/components/vg/skeleton";
import {
  Notice,
  SettingsLinkRow,
  SettingsPage,
  SettingsSection,
  SettingsSurface,
} from "@/components/settings/rows";
import { useModelStore } from "@/stores/modelStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { formatModelSize } from "@/lib/utils/format";
import { getTranslatedModelName } from "@/lib/utils/modelTranslation";
import { WritingSamplesPanel } from "../vocabulary/WritingSamplesPanel";
import {
  ModelCatalogFilters,
  type ModelFamilyOption,
} from "./ModelCatalogFilters";
import { ModelCatalogRow, type ModelRowState } from "./ModelCatalogRow";
import {
  ALL_FAMILIES,
  buildSearchIndex,
  diskUsage,
  filterModels,
  NO_FILTERS,
  visibleCatalog,
  type ModelCatalogFilterState,
} from "./modelCatalog";
import { groupModelsByFamily } from "./modelFamily";
import { useModelEngineState, useModelRowErrors } from "./useModelEngineState";

const SKELETON_GROUPS = [3, 4];

export const ModelsSettings: React.FC<{
  onOpenPrompts?: () => void;
}> = ({ onOpenPrompts }) => {
  const { t } = useTranslation();
  const [filters, setFilters] = useState<ModelCatalogFilterState>(NO_FILTERS);
  const {
    models,
    currentModel,
    downloadingModels,
    downloadProgress,
    verifyingModels,
    extractingModels,
    loading,
    error,
    isRescanning,
    downloadModel,
    cancelDownload,
    selectModel,
    deleteModel,
    rescanLocalModels,
    loadModels,
    setError,
  } = useModelStore();
  const debugMode = useSettingsStore(
    (state) => state.settings?.debug_mode ?? false,
  );
  const engine = useModelEngineState(currentModel);
  const {
    messages: rowErrorMessages,
    recordFallback: recordRowError,
    clear: clearRowError,
  } = useModelRowErrors();

  const familyFallbacks = useMemo(
    () => ({
      custom: t("settings.models.families.custom", "Added by you"),
      other: t("settings.models.families.other", "Other models"),
    }),
    [t],
  );

  const catalogModels = useMemo(() => visibleCatalog(models), [models]);

  const searchIndex = useMemo(
    () =>
      buildSearchIndex(catalogModels, {
        streaming: t("modelSelector.streaming"),
        translation: t("modelSelector.capabilities.translate"),
      }),
    [catalogModels, t],
  );

  const familyOptions = useMemo<ModelFamilyOption[]>(
    () => [
      {
        value: ALL_FAMILIES,
        label: t("settings.models.filters.allFamilies", "All families"),
      },
      ...groupModelsByFamily(catalogModels, familyFallbacks).map((group) => ({
        value: group.key,
        label: group.label,
      })),
    ],
    [catalogModels, familyFallbacks, t],
  );

  const groups = useMemo(
    () =>
      groupModelsByFamily(
        filterModels(catalogModels, filters, searchIndex, familyFallbacks),
        familyFallbacks,
      ),
    [catalogModels, filters, searchIndex, familyFallbacks],
  );

  /* Storage already in the payload: size_mb of everything on disk, legacy
   * downloads included, because they occupy the same folder. */
  const onDisk = useMemo(() => diskUsage(models), [models]);

  const rowStateOf = useCallback(
    (model: ModelInfo): ModelRowState => {
      if (model.id in extractingModels) return "extracting";
      if (model.id in verifyingModels) return "verifying";
      if (model.id in downloadingModels) return "downloading";
      if (engine.loadingModelId === model.id) return "loading";
      if (model.id === currentModel) return "active";
      return model.is_downloaded ? "downloaded" : "not-downloaded";
    },
    [
      extractingModels,
      verifyingModels,
      downloadingModels,
      engine.loadingModelId,
      currentModel,
    ],
  );

  const startDownload = useCallback(
    async (modelId: string) => {
      clearRowError(modelId);
      const started = await downloadModel(modelId);
      if (!started) {
        recordRowError(
          modelId,
          t(
            "settings.models.errors.downloadFailed",
            "The download did not start. Check your connection and try again.",
          ),
        );
      }
    },
    [downloadModel, clearRowError, recordRowError, t],
  );

  const handleDelete = useCallback(
    async (modelId: string) => {
      const model = models.find((candidate) => candidate.id === modelId);
      const modelName = model ? getTranslatedModelName(model, t) : modelId;
      const confirmed = await ask(
        modelId === currentModel
          ? t("settings.models.deleteActiveConfirm", { modelName })
          : t("settings.models.deleteConfirm", { modelName }),
        { title: t("settings.models.deleteTitle"), kind: "warning" },
      );
      if (!confirmed) return;
      clearRowError(modelId);
      await deleteModel(modelId);
    },
    [models, currentModel, deleteModel, clearRowError, t],
  );

  /* A row already shows its own failure with a retry; repeating it at the top
   * of the page would say the same thing twice. */
  const promptLibraryLink = (
    <SettingsSection label={t("prompts.title")}>
      <SettingsLinkRow
        label={t("settings.postProcessing.prompts.libraryTitle")}
        action={t("common.open")}
        onOpen={() => onOpenPrompts?.()}
      />
    </SettingsSection>
  );
  const pageError =
    error && !Object.values(rowErrorMessages).includes(error) ? error : null;

  if (loading) {
    return (
      <SettingsPage title={t("settings.models.title")}>
        <div
          role="status"
          aria-label={t("common.loading")}
          className="flex flex-col gap-8"
        >
          <div className="flex flex-wrap items-center gap-2">
            <Skeleton className="h-8 flex-1 basis-56" />
            <Skeleton className="h-8 w-44" />
            <Skeleton className="h-8 w-56" />
            <Skeleton className="h-8 w-24" />
          </div>
          {SKELETON_GROUPS.map((rows, groupIndex) => (
            <div key={groupIndex} className="flex flex-col gap-3">
              <Skeleton className="h-4 w-24" />
              <SettingsSurface>
                {Array.from({ length: rows }, (_row, rowIndex) => (
                  <div key={rowIndex} className="px-6 py-3">
                    <Skeleton className="h-5 w-full" />
                  </div>
                ))}
              </SettingsSurface>
            </div>
          ))}
        </div>
        {promptLibraryLink}
      </SettingsPage>
    );
  }

  return (
    <SettingsPage
      title={t("settings.models.title")}
      /* The one measurement the page carries, once: what the catalog costs on
       * disk. The active model is not repeated here — its own row says so,
       * and so does the sidebar chip. */
      actions={
        onDisk.count > 0 ? (
          <span className="text-[12px] tabular-nums text-gray-800">
            {t("settings.models.familyCount", "{{total}} models", {
              total: onDisk.count,
            })}
            {` \u00b7 ${formatModelSize(onDisk.sizeMb)}`}
          </span>
        ) : null
      }
    >
      <ModelCatalogFilters
        filters={filters}
        onChange={setFilters}
        familyOptions={familyOptions}
        isRescanning={isRescanning}
        onRescan={() => void rescanLocalModels()}
      />

      {pageError ? (
        <div className="flex flex-col items-start gap-2">
          <Notice tone="danger">{pageError}</Notice>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              setError(null);
              void loadModels();
            }}
          >
            {t("common.retry")}
          </Button>
        </div>
      ) : null}

      {catalogModels.length === 0 ? (
        /* The catalog ships with the app, so zero entries means the read
         * failed rather than that nothing exists yet. */
        <div className="flex flex-col items-start gap-2">
          <p className="text-sm text-gray-1000">
            {t("modelSelector.noModelsAvailable")}
          </p>
          <Notice>
            {t(
              "settings.models.emptyCatalog",
              "Sona could not read its model catalog. Rescanning picks up models already in the models folder or the Hugging Face cache.",
            )}
          </Notice>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void rescanLocalModels()}
            disabled={isRescanning}
          >
            {t("settings.models.rescan.label")}
          </Button>
        </div>
      ) : groups.length === 0 ? (
        <div className="flex flex-col items-start gap-2">
          <p className="text-sm text-gray-1000">
            {t("settings.models.noModelsMatch")}
          </p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setFilters(NO_FILTERS)}
          >
            {t("settings.models.filters.clear", "Clear filters")}
          </Button>
        </div>
      ) : (
        groups.map((group) => (
          /* One family is one microlabel over one hairline-divided surface.
           * The family's size is the number of rows under it, so it is not
           * also printed as a count. */
          <SettingsSection key={group.key} label={group.label}>
            <ul aria-label={group.label}>
              {group.models.map((model) => (
                <ModelCatalogRow
                  key={model.id}
                  model={model}
                  state={rowStateOf(model)}
                  inMemory={engine.loadedModelId === model.id}
                  percentage={downloadProgress[model.id]?.percentage}
                  error={rowErrorMessages[model.id]}
                  showQuant={debugMode}
                  onDownload={(modelId) => void startDownload(modelId)}
                  onRetry={(modelId) => void startDownload(modelId)}
                  onCancel={(modelId) => void cancelDownload(modelId)}
                  onActivate={(modelId) => void selectModel(modelId)}
                  onDelete={(modelId) => void handleDelete(modelId)}
                />
              ))}
            </ul>
          </SettingsSection>
        ))
      )}

      {promptLibraryLink}
      <WritingSamplesPanel />
    </SettingsPage>
  );
};
