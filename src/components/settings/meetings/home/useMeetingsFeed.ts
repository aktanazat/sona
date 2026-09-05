import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type MeetingCommandError,
  type MeetingHistorySummary,
  type MeetingListFilter,
  type MeetingSuggestion,
} from "@/bindings";
import { NO_MEETING_FILTER, meetingErrorKey } from "../meetingUtils";

/* One screenful of meetings, then an explicit request for older ones. The
 * backend clamps a single page at 100 rows, so paging by cursor is the only
 * way to reach a long history. */
const MEETING_PAGE_SIZE = 25;

export interface MeetingsFeed {
  suggestions: MeetingSuggestion[];
  recovery: MeetingHistorySummary[];
  meetings: MeetingHistorySummary[];
  hasMore: boolean;
  listLoading: boolean;
  page: number;
  filter: MeetingListFilter;
  homeError: string | null;
  refreshHome: () => Promise<void>;
  applyMeetingFilter: (nextFilter: MeetingListFilter) => void;
  nextMeetingPage: () => void;
  previousMeetingPage: () => void;
}

/* Everything the meetings list page reads: the page of rows on screen, the
 * position that page was fetched at, and the reads that surround the list. */
export const useMeetingsFeed = (): MeetingsFeed => {
  const { t } = useTranslation();
  const [suggestions, setSuggestions] = useState<MeetingSuggestion[]>([]);
  const [recovery, setRecovery] = useState<MeetingHistorySummary[]>([]);
  const [meetings, setMeetings] = useState<MeetingHistorySummary[]>([]);
  const [hasMore, setHasMore] = useState(false);
  /* The cursor each page past the first was fetched with, oldest-created-at
   * per step. Cursor paging has no page numbers of its own: this stack IS the
   * position, so its length is the page the person is looking at, and Newer is
   * a pop rather than a second query direction. */
  const [pageCursors, setPageCursors] = useState<number[]>([]);
  /* One truth about the list read, viewed two ways: with no rows yet it is the
   * skeleton, with rows on screen it is what disables the pager. */
  const [listLoading, setListLoading] = useState(true);
  const [listRevision, setListRevision] = useState(0);
  const [filter, setFilter] = useState<MeetingListFilter>(NO_MEETING_FILTER);
  const [homeError, setHomeError] = useState<string | null>(null);
  const homeRequestRef = useRef(0);
  const listRequestRef = useRef(0);

  /* One owner of "which page is on screen": the cursor stack and the filter
   * are the position, an effect below turns that position into a request, and
   * these handlers only move the position. Nothing is merged, because a page
   * is not an accumulation — each answer contains exactly the rows that match
   * the query, and the previous page's rows are not among them. */
  const loadMeetingPage = useCallback(
    async (cursors: number[], nextFilter: MeetingListFilter) => {
      const requestId = listRequestRef.current + 1;
      listRequestRef.current = requestId;
      setListLoading(true);
      try {
        const result = await commands.meetingList(
          cursors.length === 0 ? null : cursors[cursors.length - 1],
          MEETING_PAGE_SIZE,
          nextFilter,
        );
        if (listRequestRef.current !== requestId) return;
        if (result.status === "error") {
          setHomeError(t(meetingErrorKey(result.error)));
          return;
        }
        setMeetings(result.data.entries);
        setHasMore(result.data.has_more);
        setHomeError(null);
      } catch {
        if (listRequestRef.current === requestId) {
          setHomeError(t("meetings.errors.load"));
        }
      } finally {
        setListLoading((current) =>
          listRequestRef.current === requestId ? false : current,
        );
      }
    },
    [t],
  );

  useEffect(() => {
    void loadMeetingPage(pageCursors, filter);
  }, [filter, listRevision, loadMeetingPage, pageCursors]);

  /* A new filter is a new list, so it always lands on page one: keeping the
   * cursor would ask the store for rows older than a row the filter may have
   * just excluded. */
  const applyMeetingFilter = useCallback((nextFilter: MeetingListFilter) => {
    setFilter(nextFilter);
    setPageCursors([]);
  }, []);

  const nextMeetingPage = useCallback(() => {
    const oldest = meetings[meetings.length - 1];
    if (oldest === undefined || !hasMore) return;
    setPageCursors((current) => [...current, oldest.created_at_utc_ms]);
  }, [hasMore, meetings]);

  const previousMeetingPage = useCallback(() => {
    setPageCursors((current) => current.slice(0, -1));
  }, []);

  /* Everything on this page that is not the meetings list: what needs
   * recovering and what is being offered. The list itself belongs to the
   * position effect above, so a refresh bumps `listRevision` and lets that
   * one owner re-read it. */
  const refreshHome = useCallback(async () => {
    const requestId = homeRequestRef.current + 1;
    homeRequestRef.current = requestId;
    setListRevision((current) => current + 1);

    try {
      const [recoveryResult, suggestionsResult] = await Promise.allSettled([
        commands.meetingRecoveryList(),
        commands.meetingSuggestionsList(),
      ]);

      if (homeRequestRef.current !== requestId) return;

      const errors: MeetingCommandError[] = [];
      if (recoveryResult.status === "fulfilled") {
        if (recoveryResult.value.status === "ok") {
          setRecovery(recoveryResult.value.data);
        } else {
          errors.push(recoveryResult.value.error);
        }
      }
      if (suggestionsResult.status === "fulfilled") {
        setSuggestions(suggestionsResult.value);
      }

      if (errors.length > 0) {
        setHomeError(t(meetingErrorKey(errors[0])));
      } else if (
        recoveryResult.status === "rejected" ||
        suggestionsResult.status === "rejected"
      ) {
        setHomeError(t("meetings.errors.load"));
      }
    } catch {
      if (homeRequestRef.current === requestId) {
        setHomeError(t("meetings.errors.load"));
      }
    }
  }, [t]);

  useEffect(() => {
    void refreshHome();
  }, [refreshHome]);

  return {
    suggestions,
    recovery,
    meetings,
    hasMore,
    listLoading,
    page: pageCursors.length + 1,
    filter,
    homeError,
    refreshHome,
    applyMeetingFilter,
    nextMeetingPage,
    previousMeetingPage,
  };
};
