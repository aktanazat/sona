import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type {
  MeetingExportFormat,
  MeetingHistorySummary,
  MeetingListFilter,
} from "@/bindings";
import {
  Microlabel,
  Notice,
  SETTINGS_SURFACE,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Input } from "@/components/vg/input";
import { Skeleton } from "@/components/vg/skeleton";
import { groupByLocalDay, localDayHeading } from "@/lib/utils/localDay";
import { MeetingCard } from "./MeetingCard";
import { MeetingsPager } from "./MeetingsPager";

const MEETING_SEARCH_DEBOUNCE_MS = 200;

interface MeetingsHistoryProps {
  meetings: MeetingHistorySummary[];
  /** Meetings an interrupted launch left behind, pinned above the log. */
  recovery: MeetingHistorySummary[];
  loading: boolean;
  paging: boolean;
  hasMore: boolean;
  page: number;
  filter: MeetingListFilter;
  error: string | null;
  onOpenMeeting: (sessionId: string) => void;
  onFilterChange: (filter: MeetingListFilter) => void;
  onNextPage: () => void;
  onPreviousPage: () => void;
  onExportMeeting: (sessionId: string, format: MeetingExportFormat) => void;
  onExportLedger: (sessionId: string) => void;
  onDeleteMeeting: (sessionId: string) => void;
  /** Drop an unfinished meeting instead of finishing it. */
  onDiscardMeeting: (sessionId: string) => void;
  /** Reprocess one meeting an interrupted launch left behind. Not `onRetry`,
   *  which retries the failed read of the list itself. */
  onReprocessMeeting: (sessionId: string) => void;
  onRetry: () => void;
}

/* The wait, in the shape the rows will take: one surface, calm lines, no
 * card-per-meeting stack to collapse when the page lands. */
const MeetingListSkeleton: React.FC<{ label: string }> = ({ label }) => (
  <div role="status" aria-label={label} className={SETTINGS_SURFACE}>
    {[0, 1, 2].map((row) => (
      <div key={row} className="flex items-center gap-3 px-6 py-3.5">
        <Skeleton className="h-3.5 flex-1" />
        <Skeleton className="h-3 w-10" />
      </div>
    ))}
  </div>
);

/**
 * Meetings as a quiet log, read by day: one surface, a day's name on its own
 * line inside it, and the meetings of that day under the name.
 *
 * Round 7 closed the boxes. Every day used to open its own card, so a week of
 * meetings was seven bordered stacks and a reader counted cards instead of
 * reading titles; the day is a line now, and the whole log is one surface. The
 * two filter pickers and Clear went with them - a status a reader cannot
 * change and a window they can read off the headings are not questions worth
 * three controls - leaving the one control that finds a meeting nobody can
 * scroll to: its title.
 *
 * Unfinished meetings are the first group in the same surface rather than a
 * second list above it. The log is the only place a meeting exists, and a
 * meeting that stopped before it finished is a meeting with a reason on it,
 * not a different kind of object. Pinning it also settles what paging used to
 * decide: a meeting stranded before fifty newer ones was on page three, where
 * nobody looked for it.
 */
export const MeetingsHistory: React.FC<MeetingsHistoryProps> = ({
  meetings,
  recovery,
  loading,
  paging,
  hasMore,
  page,
  filter,
  error,
  onOpenMeeting,
  onFilterChange,
  onNextPage,
  onPreviousPage,
  onExportMeeting,
  onExportLedger,
  onDeleteMeeting,
  onDiscardMeeting,
  onReprocessMeeting,
  onRetry,
}) => {
  const { t } = useTranslation();
  const committedQuery = (filter.title_query ?? "").trim();
  const [query, setQuery] = useState(committedQuery);

  /* A typed query is the reader's ordering, not the page's: the store answers
   * it over every meeting it has, and pinning rows nobody searched for above
   * that answer would be the page arguing with the search box. */
  const pinned = committedQuery.length === 0 ? recovery : [];
  const pinnedIds = new Set(pinned.map((meeting) => meeting.session_id));
  const logged = pinnedIds.size
    ? meetings.filter((meeting) => !pinnedIds.has(meeting.session_id))
    : meetings;

  const groups = [
    ...(pinned.length === 0
      ? []
      : [
          {
            key: "unfinished",
            heading: t("meetings.recovery.title"),
            items: pinned,
          },
        ]),
    ...groupByLocalDay(logged, (meeting) => meeting.created_at_utc_ms).map(
      (day) => ({
        key: String(day.startOfDayMs),
        heading: localDayHeading(day.startOfDayMs, t),
        items: day.items,
      }),
    ),
  ];

  /* Typing narrows the list, and the store is asked once the typing stops:
   * the query is a substring match the backend runs, so a keystroke is a
   * round trip. */
  useEffect(() => {
    if (query.trim() === committedQuery) return;
    const timer = window.setTimeout(() => {
      onFilterChange({ ...filter, title_query: query.trim() });
    }, MEETING_SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [committedQuery, filter, onFilterChange, query]);

  return (
    <section className="flex flex-col gap-2">
      <div className="flex min-h-8 items-center justify-between gap-4">
        <h2>
          <Microlabel>{t("meetings.history.title")}</Microlabel>
        </h2>
        {/* Nothing recorded and nothing searched: a search box over a list
         * that does not exist yet is one control too many. It arrives with
         * the first meeting. */}
        {groups.length === 0 &&
        committedQuery.length === 0 &&
        !loading ? null : (
          <Input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            aria-label={t("meetings.list.searchLabel")}
            placeholder={t("meetings.list.searchPlaceholder")}
            className="h-8 w-40 text-[13px] sm:w-56"
          />
        )}
      </div>

      {error ? (
        <div className="flex flex-wrap items-center gap-3">
          <Notice tone="danger">{error}</Notice>
          <Button type="button" variant="outline" size="sm" onClick={onRetry}>
            {t("meetings.actions.retry")}
          </Button>
        </div>
      ) : null}

      <div data-slot="meeting-list-region" className="flex flex-col gap-3">
        {loading ? (
          <MeetingListSkeleton label={t("meetings.history.loading")} />
        ) : groups.length === 0 ? (
          /* Absence, said once, in one line: no card, no illustration, no
           * second invitation to press the Record that is already on screen
           * above. */
          <p className="text-[13px] leading-5 text-gray-800">
            {committedQuery.length === 0
              ? t("meetings.history.emptyTitle")
              : t("meetings.list.noMatches", { query: committedQuery })}
          </p>
        ) : (
          <div className={SETTINGS_SURFACE}>
            {groups.map((group) => (
              <React.Fragment key={group.key}>
                <h3 data-slot="meeting-day" className="px-6 pt-2.5 pb-2">
                  <Microlabel>{group.heading}</Microlabel>
                </h3>
                <ul
                  role="list"
                  aria-label={group.heading}
                  className="divide-y divide-gray-alpha-400"
                >
                  {group.items.map((meeting) => (
                    <MeetingCard
                      key={meeting.session_id}
                      meeting={meeting}
                      onOpen={() => onOpenMeeting(meeting.session_id)}
                      onExport={(format) =>
                        onExportMeeting(meeting.session_id, format)
                      }
                      onExportLedger={() => onExportLedger(meeting.session_id)}
                      onDelete={() => onDeleteMeeting(meeting.session_id)}
                      onDiscard={() => onDiscardMeeting(meeting.session_id)}
                      onRetry={() => onReprocessMeeting(meeting.session_id)}
                    />
                  ))}
                </ul>
              </React.Fragment>
            ))}
          </div>
        )}

        <MeetingsPager
          loading={loading}
          paging={paging}
          hasMore={hasMore}
          page={page}
          onNextPage={onNextPage}
          onPreviousPage={onPreviousPage}
        />
      </div>
    </section>
  );
};
