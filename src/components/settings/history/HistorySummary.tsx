import React from "react";
import { useTranslation } from "react-i18next";
import type { HistoryStats } from "@/bindings";
import { Button } from "@/components/vg/button";
import { Skeleton } from "@/components/vg/skeleton";
import { formatDurationShort } from "@/lib/utils/format";

interface HistorySummaryProps {
  stats: HistoryStats | null;
  loading: boolean;
  error: boolean;
  onRetry: () => void;
}

/* The line every metadata readout on this page is written in: the brief's Meta
 * tier, tabular, secondary. `snap-measured` because these are measurements — a
 * tweened count displays a number the backend never reported. */
const SUMMARY_LINE =
  "snap-measured min-h-5 text-[13px] leading-[18px] text-gray-900 tabular-nums";

/* All-time totals as one sentence: "5 recordings · 3m 39s · 587 words".
 *
 * This was three stat cards with a microlabel over a 24px figure, which gave
 * the page's loudest typography to numbers nobody came here to read — the log
 * below is what the page is for. One line states the same three facts and
 * spends no vertical space. No per-source split: provenance belongs to a
 * recording, and the row that owns it states it on its receipt.
 *
 * Exported because the line is the page's one derived readout and the whole
 * page cannot be rendered without its data effects. */
export const HistorySummary: React.FC<HistorySummaryProps> = ({
  stats,
  loading,
  error,
  onRetry,
}) => {
  const { t } = useTranslation();

  if (error) {
    return (
      <div className="flex min-h-5 flex-wrap items-center gap-3">
        <p className="text-[13px] leading-[18px] text-red-900">
          {t("settings.history.stats.unavailable")}
        </p>
        {/* Bordered, not a text ghost: this line has no banner surface of its
         * own, so a ghost label would read as the tail of the sentence beside
         * it rather than as the control that refills it. */}
        <Button variant="outline" size="sm" onClick={onRetry}>
          {t("settings.history.retry")}
        </Button>
      </div>
    );
  }

  if (stats === null) {
    if (!loading) {
      return (
        <p className={SUMMARY_LINE} data-testid="history-summary">
          {t("settings.history.stats.unavailable")}
        </p>
      );
    }
    /* One bar the width the sentence will be. Labels are not printed over it:
     * the totals only read as a sentence together, and half a sentence with
     * three blanks in it is noisier than the bar. */
    return (
      <div
        role="status"
        aria-label={t("libraryV2.summaryLoading")}
        className="flex min-h-5 items-center"
        data-testid="history-summary-loading"
      >
        <Skeleton className="h-3 w-56" />
      </div>
    );
  }

  /* Nothing recorded, nothing to total: "0 recordings · 0s · 0 words" is the
   * empty state written as machinery, and the list below already says it in a
   * sentence. */
  if (stats.entries === 0) return null;

  const cells = [
    t("libraryV2.recordings", { count: stats.entries }),
    formatDurationShort(stats.total_duration_ms / 1000),
    t("libraryV2.words", { count: stats.total_words }),
  ];

  return (
    <p className={SUMMARY_LINE} data-testid="history-summary">
      {cells.join(" · ")}
    </p>
  );
};
