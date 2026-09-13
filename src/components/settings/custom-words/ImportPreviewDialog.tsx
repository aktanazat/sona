import React from "react";
import { useTranslation } from "react-i18next";
import type { VocabularyCsvPreview } from "@/bindings";
import { Button } from "@/components/vg/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { FactChip, Notice } from "@/components/settings/rows";
import { ColumnHeader } from "../vocabulary/PanelParts";

export interface ImportReview {
  csv: string;
  preview: VocabularyCsvPreview;
  step: "review" | "confirm";
}

interface ImportPreviewDialogProps {
  review: ImportReview | null;
  savedCount: number;
  saving: boolean;
  onStep: (step: "review" | "confirm") => void;
  onClose: () => void;
  onApply: () => void;
}

/**
 * Two steps, because a CSV import replaces the saved list: read what the file
 * contains, then confirm the replacement. Nothing is written until the last
 * button.
 */
export const ImportPreviewDialog: React.FC<ImportPreviewDialogProps> = ({
  review,
  savedCount,
  saving,
  onStep,
  onClose,
  onApply,
}) => {
  const { t } = useTranslation();
  const preview = review?.preview ?? null;
  const reviewing = review?.step !== "confirm";

  /* Only the count that always means something, plus the ones that are a
   * reason to stop. A row of zeroes is noise the table below already denies. */
  const counts = preview
    ? [
        {
          label: t("settings.advanced.customWords.importPreview.valid"),
          value: preview.valid_rows,
        },
        {
          label: t("settings.advanced.customWords.importPreview.invalid"),
          value: preview.invalid_rows,
        },
        {
          label: t("settings.advanced.customWords.importPreview.duplicates"),
          value: preview.duplicate_rows,
        },
        {
          label: t("settings.advanced.customWords.importPreview.conflicts"),
          value: preview.conflict_rows,
        },
      ].filter((count, index) => index === 0 || count.value > 0)
    : [];

  return (
    <Dialog
      open={review !== null}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {t("settings.advanced.customWords.importPreview.title")}
          </DialogTitle>
          <DialogDescription>
            {t("settings.advanced.customWords.importPreview.description")}
          </DialogDescription>
        </DialogHeader>

        {preview && (
          <div className="flex flex-col gap-3">
            {reviewing ? (
              <>
                <div className="flex flex-wrap gap-x-5 gap-y-1.5">
                  {counts.map((count) => (
                    <FactChip
                      key={count.label}
                      label={count.label}
                      value={count.value}
                    />
                  ))}
                </div>

                {!preview.can_apply && (
                  <Notice tone="danger">
                    {t("settings.advanced.customWords.importPreview.blocked")}
                  </Notice>
                )}

                {preview.entries.length > 0 && (
                  <div className="max-h-56 overflow-y-auto rounded-card border border-gray-alpha-400">
                    <ColumnHeader
                      gridClassName="grid grid-cols-2 gap-2"
                      labels={[
                        t("settings.advanced.customWords.spoken"),
                        t("settings.advanced.customWords.written"),
                      ]}
                    />
                    <ul
                      role="list"
                      className="divide-y divide-gray-alpha-400 text-[12.5px]"
                    >
                      {preview.entries.map((entry) => (
                        <li
                          key={`${entry.spoken}\u0000${entry.written}`}
                          className="grid grid-cols-2 gap-2 px-4 py-1.5"
                        >
                          <span className="truncate text-gray-1000">
                            {entry.spoken}
                          </span>
                          <span className="truncate text-gray-700">
                            {entry.written}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </>
            ) : (
              <p className="text-[14px] leading-[21px] text-pretty text-gray-900">
                {t(
                  "settings.advanced.customWords.importPreview.replaceSummary",
                  {
                    defaultValue:
                      "Applying adds the {{importedCount}} pairs from this file and keeps the {{savedCount}} saved pairs.",
                    savedCount,
                    importedCount: preview.entries.length,
                  },
                )}
              </p>
            )}
          </div>
        )}

        <DialogFooter>
          {reviewing ? (
            <>
              <Button
                variant="outline"
                size="sm"
                onClick={onClose}
                disabled={saving}
              >
                {t("common.cancel")}
              </Button>
              <Button
                size="sm"
                onClick={() => onStep("confirm")}
                disabled={saving || !preview?.can_apply}
                data-testid="vocabulary-import-continue"
              >
                {t(
                  "settings.advanced.customWords.importPreview.continue",
                  "Continue",
                )}
              </Button>
            </>
          ) : (
            <>
              <Button
                variant="outline"
                size="sm"
                onClick={() => onStep("review")}
                disabled={saving}
              >
                {t("settings.advanced.customWords.importPreview.back", "Back")}
              </Button>
              <Button
                size="sm"
                onClick={onApply}
                disabled={saving || !preview?.can_apply}
                data-testid="vocabulary-import-apply"
              >
                {t("settings.advanced.customWords.importPreview.apply")}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};
