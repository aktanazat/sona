import React from "react";
import { Ellipsis } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { MeetingExportFormat, MeetingHistorySummary } from "@/bindings";
import { Button } from "@/components/vg/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import { Microlabel } from "@/components/settings/rows";
import { cn } from "@/lib/cn";
import { formatDurationShort } from "@/lib/utils/format";
import { formatTimeOfDay } from "@/lib/utils/localDay";
import { processingStatusKey } from "../meetingUtils";
import { meetingCardStatus } from "./meetingCardStatus";

/* Every row that can be acted on keeps its actions behind this one glyph at
 * the end of the row, the same menu on the meetings list and on a person's
 * meetings.
 *
 * Round 7 unpinned it: a column of outlined buttons down a list of documents
 * is a column of chrome, so the glyph appears on hover, on keyboard focus
 * anywhere in the row, and while its own menu is open. Opacity rather than
 * `hidden`, because the trigger has to stay in the tab order for the reader
 * who never touches a mouse. */
const RowActionsMenu: React.FC<{
  label: string;
  children: React.ReactNode;
}> = ({ label, children }) => (
  <DropdownMenu>
    <DropdownMenuTrigger asChild>
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        className={cn(
          "flex-none text-gray-800 opacity-0 transition-opacity hover:text-gray-1000",
          "group-hover:opacity-100 group-focus-within:opacity-100",
          "focus-visible:opacity-100 data-[state=open]:opacity-100",
          "motion-reduce:transition-none",
        )}
        aria-label={label}
        title={label}
      >
        <Ellipsis aria-hidden="true" />
      </Button>
    </DropdownMenuTrigger>
    <DropdownMenuContent align="end" className="min-w-52">
      {children}
    </DropdownMenuContent>
  </DropdownMenu>
);

interface MeetingSummaryRowProps
  extends Omit<React.ComponentProps<"li">, "children" | "title"> {
  title: string;
  headline: React.ReactNode | null;
  footerLeading?: React.ReactNode;
  metadata?: React.ReactNode[];
  actionsLabel: string;
  actions: React.ReactNode;
}

export const MeetingSummaryRow: React.FC<MeetingSummaryRowProps> = ({
  title,
  headline,
  footerLeading,
  metadata = [],
  actionsLabel,
  actions,
  className,
  ...props
}) => (
  <li className={cn("group flex items-start gap-2", className)} {...props}>
    <div className="flex min-w-0 flex-1 flex-col gap-1 text-start">
      <span className="truncate text-[14px] leading-[21px] font-medium text-gray-1000">
        {title}
      </span>
      {headline === null ? null : (
        <span className="w-full truncate text-[14px] leading-[21px] text-gray-900">
          {headline}
        </span>
      )}
      {footerLeading === undefined && metadata.length === 0 ? null : (
        <span className="flex w-full min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-2">
          {footerLeading}
          {metadata.length === 0 ? null : (
            <span
              data-slot="meeting-facts"
              className="snap-measured ms-auto flex flex-none items-center text-[13px] leading-[18px] text-gray-900 tabular-nums"
            >
              {metadata.map((fact, index) => (
                <React.Fragment key={index}>
                  {index === 0 ? null : (
                    <span aria-hidden="true" className="px-1.5 text-gray-700">
                      ·
                    </span>
                  )}
                  {fact}
                </React.Fragment>
              ))}
            </span>
          )}
        </span>
      )}
    </div>

    <RowActionsMenu label={actionsLabel}>{actions}</RowActionsMenu>
  </li>
);

interface MeetingCardProps {
  meeting: MeetingHistorySummary;
  onOpen: () => void;
  onExport: (format: MeetingExportFormat) => void;
  onExportLedger: () => void;
  onDelete: () => void;
  /** Drop an unfinished meeting instead of finishing it. The list offers
   *  this in place of Delete, because a session that never finished is not a
   *  document to file in the bin. */
  onDiscard: () => void;
  /** Reprocess a meeting an interrupted launch left behind. */
  onRetry: () => void;
}

/* One recorded meeting inside its day group: the clock time in the gutter, the
 * title, and how long it ran.
 *
 * Round 7 emptied the rest of the row. The day heading above the group owns
 * the date, so the "5 hours ago" cell was the same fact in weaker words; the
 * speaker labels were the diarizer reporting on itself; and "Live", "Ready"
 * and "Processing" were the machinery's own vocabulary, which nobody reading a
 * list of meetings has to act on. What survives is the one state that asks the
 * reader for something - a meeting that ended badly says why, in red, on the
 * title's line - and Try again, which appears only where it can actually run.
 * The summary the row used to print lives on the meeting itself, one click
 * away, and stays here as the row's hover title. */
export const MeetingCard: React.FC<MeetingCardProps> = ({
  meeting,
  onOpen,
  onExport,
  onExportLedger,
  onDelete,
  onDiscard,
  onRetry,
}) => {
  const { t } = useTranslation();
  const headline = meeting.headline ?? { kind: "none" };
  const recordedMs = meeting.recorded_duration_ms ?? null;
  const processing = meeting.processing_status;
  const openTitle =
    headline.kind === "ledger" || headline.kind === "summary"
      ? `${meeting.title} — ${headline.text}`
      : meeting.title;
  /* The one word a row is allowed to say about its own state, and only when
   * the reader has to do something about it. A meeting that ended badly says
   * why, because "Needs attention" is a state and not an explanation; the
   * state word is the fallback for the meeting an interrupted launch left
   * behind with nothing recorded against it.
   *
   * The two carry different colours because they ask for different things.
   * A processing failure is over and red says so. "Needs attention" is a
   * meeting still waiting for a press, and red on a row the reader can fix
   * by pressing the button next to it reads as damage instead of an errand. */
  const attention =
    meetingCardStatus(meeting.phase, processing) !== "needs_attention"
      ? null
      : processing.kind === "failed" || processing.kind === "cancelled"
        ? { text: t(processingStatusKey(processing)), tone: "text-red-900" }
        : {
            text: t("meetings.list.status.needs_attention"),
            tone: "text-amber-900",
          };

  return (
    <li
      data-slot="meeting-entry"
      data-headline={headline.kind}
      className="group flex items-center gap-1 pe-4"
    >
      <button
        type="button"
        onClick={onOpen}
        title={openTitle}
        className="hover-fast flex min-w-0 flex-1 items-baseline gap-5 px-6 py-3.5 text-start hover:bg-background-200"
      >
        <Microlabel className="w-[62px] flex-none text-end tabular-nums">
          {formatTimeOfDay(meeting.created_at_utc_ms)}
        </Microlabel>
        <span className="flex min-w-0 flex-1 items-baseline gap-2">
          <span className="truncate text-[14px] leading-[21px] font-medium text-gray-1000">
            {meeting.title}
          </span>
          {attention === null ? null : (
            <span
              data-slot="meeting-attention"
              className={cn(
                "flex-none text-[13px] leading-[18px]",
                attention.tone,
              )}
            >
              {attention.text}
            </span>
          )}
        </span>
        {recordedMs === null ? null : (
          <span className="snap-measured flex-none text-[13px] leading-[18px] text-gray-800 tabular-nums">
            {formatDurationShort(recordedMs / 1000)}
          </span>
        )}
      </button>

      {meeting.phase === "recovery_required" ? (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="flex-none"
          onClick={onRetry}
        >
          {t("meetings.actions.retry")}
        </Button>
      ) : null}

      <RowActionsMenu label={t("meetings.list.rowActions")}>
        <DropdownMenuItem onSelect={() => onExport("markdown")}>
          {t("meetings.list.exportMarkdown")}
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => onExport("json")}>
          {t("meetings.list.exportJson")}
        </DropdownMenuItem>
        {headline.kind === "ledger" ? (
          <DropdownMenuItem onSelect={onExportLedger}>
            {t("meetings.list.exportLedger")}
          </DropdownMenuItem>
        ) : null}
        <DropdownMenuSeparator />
        {meeting.phase === "recovery_required" ? (
          <DropdownMenuItem variant="destructive" onSelect={onDiscard}>
            {t("meetings.actions.discard")}
          </DropdownMenuItem>
        ) : (
          <DropdownMenuItem variant="destructive" onSelect={onDelete}>
            {t("meetings.actions.delete")}
          </DropdownMenuItem>
        )}
      </RowActionsMenu>
    </li>
  );
};
