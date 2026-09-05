import React from "react";
import { useTranslation } from "react-i18next";
import type { PersonSummary } from "@/bindings";
import { cn } from "@/lib/cn";
import { CardBand } from "@/components/settings/CardBand";
import { SETTINGS_CARD } from "@/components/settings/rows";
import { formatEntryTimestamp } from "@/lib/utils/format";

/* Three sentences about a relationship, under a band that names them.
 *
 * "About" and nothing else: the paragraph reads as the answer to that one
 * word, so it needs no section label above the card as well. The engine and
 * the time stay under it because a paragraph a model wrote is only readable if
 * you know which model and when — the same reason the row stores both.
 *
 * No paragraph, no card. The card used to stand empty to hold the button that
 * asks for one; that verb is in the page's menu now, where it can be pressed
 * whether or not there is anything here yet. */
export const PersonSummarySection: React.FC<{
  summary: PersonSummary | null;
}> = ({ summary }) => {
  const { t } = useTranslation();
  if (summary === null) return null;

  return (
    <section
      data-slot="person-summary"
      className={cn(SETTINGS_CARD, "overflow-hidden")}
    >
      <CardBand as="h2" title={t("people.summary.title")} />
      <div className="flex flex-col gap-3 px-6 py-5">
        <p className="text-[16px] leading-[25px] text-gray-1000 text-pretty">
          {summary.text}
        </p>
        <span className="text-[13px] leading-[18px] text-gray-900 tabular-nums">
          {t("people.summary.provenance", {
            model: summary.model_id,
            date: formatEntryTimestamp(summary.generated_at_utc_ms),
          })}
        </span>
      </div>
    </section>
  );
};
