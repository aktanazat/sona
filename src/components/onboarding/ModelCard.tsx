import React from "react";
import { useTranslation } from "react-i18next";
import type { ModelInfo } from "@/bindings";
import { formatModelSize } from "../../lib/utils/format";
import {
  getTranslatedModelDescription,
  getTranslatedModelName,
} from "../../lib/utils/modelTranslation";
import { Button } from "@/components/vg/button";

export type ModelCardStatus =
  | "downloadable"
  | "downloading"
  | "verifying"
  | "extracting"
  | "switching"
  | "available";

interface ModelCardProps {
  model: ModelInfo;
  status?: ModelCardStatus;
  disabled?: boolean;
  onSelect: (modelId: string) => void;
  onDownload?: (modelId: string) => void;
  onCancel?: (modelId: string) => void;
  downloadProgress?: number;
}

/**
 * One model, as a thing you pick: its name, the one sentence that says what
 * picking it costs you, and its download size.
 *
 * What used to be here: two labelled score meters for accuracy and speed, a
 * capability strip of three icon-and-word pairs (languages, translate,
 * streaming), a size line behind a disk or download glyph, up to five badges,
 * and a delete button — on a screen whose whole job is to get one model onto
 * the disk. The scores restated the description ("Good accuracy, but slow"),
 * the capability strip restated it again, and the glyphs sat beside words that
 * already said it. Everything that survived is something a first-run reader
 * acts on.
 */
const ModelCard: React.FC<ModelCardProps> = ({
  model,
  status = "downloadable",
  disabled = false,
  onSelect,
  onDownload,
  onCancel,
  downloadProgress,
}) => {
  const { t } = useTranslation();
  const isClickable = status === "available" || status === "downloadable";

  const handleClick = () => {
    if (!isClickable || disabled) return;
    if (status === "downloadable" && onDownload) {
      onDownload(model.id);
    } else {
      onSelect(model.id);
    }
  };

  /* One line for every kind of work this card can be doing, so the card has one
   * shape whether the bytes are arriving, being checked, being unpacked, or the
   * model is being loaded. Only a download knows how far along it is; the rest
   * are indeterminate and say so by moving. */
  const percentage = status === "downloading" ? (downloadProgress ?? 0) : null;
  const workLabel =
    percentage !== null
      ? t("modelSelector.downloading", { percentage: Math.round(percentage) })
      : status === "verifying"
        ? t("modelSelector.verifyingGeneric")
        : status === "extracting"
          ? t("modelSelector.extractingGeneric")
          : status === "switching"
            ? t("modelSelector.switching")
            : null;

  return (
    <div
      onClick={handleClick}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing || !isClickable) return;
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          handleClick();
        }
      }}
      role={isClickable ? "button" : undefined}
      tabIndex={isClickable ? 0 : undefined}
      aria-disabled={isClickable && disabled ? true : undefined}
      className="ob-card"
      data-busy={workLabel ? "true" : undefined}
      data-disabled={isClickable && disabled ? "true" : undefined}
    >
      <div className="ob-card-head">
        <h3 className="ob-card-name">{getTranslatedModelName(model, t)}</h3>
        {status === "downloadable" && (
          <span className="ob-card-size">
            {formatModelSize(Number(model.size_mb))}
          </span>
        )}
      </div>
      <p className="ob-card-line">{getTranslatedModelDescription(model, t)}</p>

      {workLabel && (
        <div className="ob-card-work">
          {/* A bar only where there is something to measure: a download knows
              its own length, and verifying, unpacking and loading do not. Those
              three say so in a word rather than drawing a bar that would be
              inventing a position, or animating one that means nothing. */}
          {percentage !== null && (
            <div className="ob-card-track">
              <div
                className="ob-card-fill"
                style={{ inlineSize: `${percentage}%` }}
              />
            </div>
          )}
          <span className="ob-card-work-label">{workLabel}</span>
          {status === "downloading" && onCancel && (
            <Button
              variant="ghost"
              size="sm"
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onCancel(model.id);
              }}
              aria-label={t("modelSelector.cancelDownload")}
            >
              {t("common.cancel")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
};

export default ModelCard;
