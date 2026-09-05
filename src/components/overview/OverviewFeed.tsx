import React, { useCallback, useEffect, useRef, useState } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  commands,
  events,
  type LearningDecisionStatus,
  type LearningSuggestion,
  type LearningSuggestionEntry,
  type PersonOpenLoop,
  type UpdateCheckResult,
  type WorkflowRunCursor,
  type WorkflowRunReceipt,
} from "@/bindings";
import { useLearningDecisions } from "@/hooks/useLearningDecisions";
import { Button } from "@/components/vg/button";
import { cn } from "@/lib/cn";
import { formatRelativeTime } from "@/lib/utils/format";
import {
  Microlabel,
  SettingsCard,
  SettingsDisclosure,
} from "@/components/settings/rows";
import {
  formatWorkflowOutcome,
  workflowOutcomeHasEffect,
} from "@/components/settings/workflows/formatWorkflowOutcome";
import { WORKFLOW_NAME_KEY } from "@/components/settings/workflows/workflowCatalogue";

/* The two lists under the hero.
 *
 * "Needs you" is everything on this page that asks the reader a question — a
 * promise they made, a habit Sona wants permission to learn, a release they
 * are not on yet. "Recent" is what Sona did without asking, closed by default,
 * because a reader checks it, they do not work from it. Both are written in the
 * same row grammar: a line of text, one meta line under it, and the surface
 * around them drawing the only border. */

const OVERVIEW_RUN_PAGE_SIZE = 20;
const OVERVIEW_RECEIPT_LIMIT = 3;

/* What the recent list will show: a run that succeeded, changed something, and
 * that the reader can act on, which normally means it names a meeting to open.
 * Meeting-recording runs are the exception — skipping a detected meeting
 * leaves no session to open, and "Skipped recording a detected meeting" is
 * exactly the line a reader needs to see. A run that found nothing keeps its
 * row in the full run log under Settings, where a quiet pass is the point. */
const belongsInFeed = (receipt: WorkflowRunReceipt): boolean =>
  receipt.status === "ok" &&
  workflowOutcomeHasEffect(receipt) &&
  (receipt.jump_target?.kind === "meeting" ||
    receipt.workflow_id === "meeting_activity");

const loadRecentFeedReceipts = async (): Promise<WorkflowRunReceipt[]> => {
  const receipts: WorkflowRunReceipt[] = [];
  let cursor: WorkflowRunCursor | null = null;

  do {
    const result = await commands.workflowRuns({
      workflow_id: null,
      cursor,
      limit: OVERVIEW_RUN_PAGE_SIZE,
    });
    if (result.status === "error") throw new Error(result.error);
    for (const receipt of result.data.entries) {
      if (belongsInFeed(receipt)) {
        receipts.push(receipt);
        if (receipts.length === OVERVIEW_RECEIPT_LIMIT) return receipts;
      }
    }
    cursor = result.data.next_cursor;
  } while (cursor !== null);

  return receipts;
};

export type FeedState<T> =
  | { status: "loading" }
  | { status: "loaded"; entries: readonly T[] }
  | { status: "error" };

const ROW_TITLE = "block text-[14px] leading-[21px] font-medium text-gray-1000";
const ROW_META = "mt-1 block text-[13px] leading-[18px] text-gray-900";
const ROW_BOX = "px-6 py-3.5";
const ROW_SPLIT =
  "flex flex-wrap items-center justify-between gap-x-3 gap-y-2 px-6 py-3.5";
const ROW_LINK =
  "hover-fast w-full px-6 py-3.5 text-start hover:bg-gray-alpha-100 focus-visible:ring-2 focus-visible:ring-focus-ring focus-visible:outline-none";
/* One quiet line where a list would be: what is not there, and what puts it
 * there. Never a blank surface. */
const EMPTY_LINE = "text-[13px] leading-5 text-gray-800";

/* The source and the time, as one meta line with a tabular clock. */
const RowMeta: React.FC<{
  source: React.ReactNode;
  time: string;
  className?: string;
}> = ({ source, time, className }) => (
  <span className={cn(ROW_META, className)}>
    {source}
    <span aria-hidden="true"> · </span>
    <span className="tabular-nums">{time}</span>
  </span>
);

const ListStateRow: React.FC<{
  status: "loading" | "error";
  onRetry: () => void;
}> = ({ status, onRetry }) => {
  const { t } = useTranslation();

  if (status === "loading") {
    return (
      <div
        role="status"
        className={cn(ROW_BOX, "text-[14px] leading-[21px] text-gray-900")}
      >
        {t("common.loading")}
      </div>
    );
  }

  return (
    <div
      role="alert"
      className={cn(
        ROW_BOX,
        "flex items-center justify-between gap-3 text-[14px] leading-[21px] text-gray-900",
      )}
    >
      <span>
        {t("overview.needsYou.loadError", "Couldn't load this list.")}
      </span>
      <Button type="button" variant="ghost" size="xs" onClick={onRetry}>
        {t("common.retry")}
      </Button>
    </div>
  );
};

/* What Sona noticed, phrased as a question a person can answer. Every line is
 * one mined suggestion with its own evidence and its own answer. Nothing is
 * filtered here: the store applies every cap and floor before a row exists, so
 * whatever arrives is meant to be read. */
const headline = (suggestion: LearningSuggestion, t: TFunction): string => {
  switch (suggestion.kind) {
    case "spoken_punctuation":
      return t("learningV2.feed.spokenPunctuation", {
        spoken: suggestion.spoken,
        written: suggestion.written,
      });
    case "vocabulary_correction":
      return t("learningV2.feed.vocabularyCorrection", {
        spoken: suggestion.spoken,
        written: suggestion.written,
      });
    case "mode_habit":
      return t("learningV2.feed.modeHabit", { mode: suggestion.mode_name });
    case "capture_advice":
      switch (suggestion.advice) {
        case "retry_rate":
          return t("learningV2.feed.retryRate", {
            subject: suggestion.subject,
            times: (suggestion.stat_permille / 1000).toFixed(1),
          });
        case "lost_capture_rate":
          return t("learningV2.feed.lostCaptureRate", {
            subject: suggestion.subject,
            times: (suggestion.stat_permille / 1000).toFixed(1),
          });
        case "input_level":
          return t("learningV2.feed.inputLevel", {
            percent: Math.round(suggestion.stat_permille / 10),
          });
        default: {
          const exhaustive: never = suggestion.advice;
          return exhaustive;
        }
      }
    default: {
      const exhaustive: never = suggestion;
      return exhaustive;
    }
  }
};

/* What the store remembers as the answer's subject.
 *
 * Two of these are not display-only: loop 4 primes a session's ASR from the
 * accepted vocabulary lines (`accepted_display_texts_in`), so those stay the
 * bare term the reader agreed to and never a rendered sentence. Advice is the
 * one kind nothing reads back, and the one whose discriminant — `retry_rate` —
 * is not a thing to show a person, so it carries the line they actually saw. */
const displayText = (suggestion: LearningSuggestion, t: TFunction): string => {
  switch (suggestion.kind) {
    case "spoken_punctuation":
    case "vocabulary_correction":
      return suggestion.spoken;
    case "mode_habit":
      return suggestion.mode_name;
    case "capture_advice":
      return headline(suggestion, t);
    default: {
      const exhaustive: never = suggestion;
      return exhaustive;
    }
  }
};

const loadSuggestions = async (): Promise<
  readonly LearningSuggestionEntry[]
> => {
  const result = await commands.learningSuggestions();
  return result.status === "ok" ? result.data.entries : [];
};

export interface LearningRows {
  /** Null until the suggestion read answers. */
  entries: readonly LearningSuggestionEntry[] | null;
  answer: (
    entry: LearningSuggestionEntry,
    status: LearningDecisionStatus,
  ) => void;
}

/** The mined suggestions, and the write that answers one. */
export const useLearningRows = (): LearningRows => {
  const { t } = useTranslation();
  const { entries, decide } = useLearningDecisions(loadSuggestions);

  return {
    entries,
    answer: (entry, status) =>
      void decide(
        {
          loop_kind: entry.loop_kind,
          candidate_key: entry.candidate_key,
          display_text: displayText(entry.suggestion, t),
        },
        status,
      ),
  };
};

export interface OverviewFeed {
  receipts: FeedState<WorkflowRunReceipt>;
  openLoops: FeedState<PersonOpenLoop>;
  refresh: () => void;
}

/**
 * Both backend lists on one refresh and one set of listeners.
 *
 * They are drawn in two different places on the page — what needs the reader,
 * and what Sona already did — but they answer to the same three events, so
 * splitting the read would double the subscriptions for nothing.
 */
export const useOverviewFeed = (): OverviewFeed => {
  const [receipts, setReceipts] = useState<FeedState<WorkflowRunReceipt>>({
    status: "loading",
  });
  const [openLoops, setOpenLoops] = useState<FeedState<PersonOpenLoop>>({
    status: "loading",
  });
  const requestGenerationRef = useRef(0);

  const refresh = useCallback(async () => {
    const requestGeneration = requestGenerationRef.current + 1;
    requestGenerationRef.current = requestGeneration;
    setReceipts({ status: "loading" });
    setOpenLoops({ status: "loading" });

    const [receiptResult, inboxResult] = await Promise.allSettled([
      loadRecentFeedReceipts(),
      commands.openLoopsInbox(5),
    ]);
    if (requestGenerationRef.current !== requestGeneration) return;

    setReceipts(
      receiptResult.status === "fulfilled"
        ? { status: "loaded", entries: receiptResult.value }
        : { status: "error" },
    );
    setOpenLoops(
      inboxResult.status === "fulfilled" && inboxResult.value.status === "ok"
        ? { status: "loaded", entries: inboxResult.value.data.entries }
        : { status: "error" },
    );
  }, []);

  useEffect(() => {
    void refresh();
    const subscriptions = Promise.all([
      events.meetingArtifactChanged.listen(() => void refresh()),
      events.meetingRemoved.listen(() => void refresh()),
      events.historyUpdatePayload.listen(() => void refresh()),
    ]);

    return () => {
      requestGenerationRef.current += 1;
      void subscriptions.then((unlisteners) => {
        for (const unlisten of unlisteners) unlisten();
      });
    };
  }, [refresh]);

  return { receipts, openLoops, refresh: () => void refresh() };
};

export interface NeedsYouProps {
  openLoops: FeedState<PersonOpenLoop>;
  /** The mined suggestions, or null until that read has answered. */
  learning: readonly LearningSuggestionEntry[] | null;
  /** The release to offer, or null when there is none to offer: nothing newer,
   * a check that failed, or a row the reader closed. */
  update: UpdateCheckResult | null;
  /** Whether the check has come back at all. A check still out is not the
   * same answer as a check that found nothing. */
  updateChecked: boolean;
  onAnswerLearning: (
    entry: LearningSuggestionEntry,
    status: LearningDecisionStatus,
  ) => void;
  onDismissUpdate: () => void;
  onOpenMeeting: (meetingId: string) => void;
  onRetry: () => void;
  nowMs?: number;
}

/**
 * Everything on this page that asks the reader for something, as one list.
 *
 * "Nothing needs you." is only printed once every source has actually
 * answered: while a read is in flight the section draws nothing, and a read
 * that failed keeps its own row, because an empty list and an unread list are
 * not the same claim.
 */
export const NeedsYou: React.FC<NeedsYouProps> = ({
  openLoops,
  learning,
  update,
  updateChecked,
  onAnswerLearning,
  onDismissUpdate,
  onOpenMeeting,
  onRetry,
  nowMs = Date.now(),
}) => {
  const { t } = useTranslation();
  const updateWaiting =
    update !== null && update.status === "update_available" ? update : null;
  const releaseUrl = updateWaiting?.url ?? null;

  if (openLoops.status === "loading") return null;

  const loops = openLoops.status === "loaded" ? openLoops.entries : [];
  const suggestions = learning ?? [];
  const nothingToShow =
    openLoops.status === "loaded" &&
    loops.length === 0 &&
    suggestions.length === 0 &&
    updateWaiting === null;

  if (nothingToShow) {
    /* The other two sources answer on their own clock - the suggestion read,
     * and the update check that waits for the first painted frame - so an
     * empty page is not yet the claim this sentence makes. Until both have
     * answered, the section draws nothing rather than a line it would have to
     * take back a moment later. */
    if (learning === null || !updateChecked) return null;

    return (
      <p className={EMPTY_LINE}>
        {t("overview.needsYou.empty", "Nothing needs you.")}
      </p>
    );
  }

  return (
    <div className="flex min-w-0 flex-col gap-2">
      <h2 id="overview-needs-you">
        <Microlabel>{t("overview.needsYou.title", "Needs you")}</Microlabel>
      </h2>
      <SettingsCard
        aria-labelledby="overview-needs-you"
        className="overflow-hidden"
      >
        {openLoops.status === "error" ? (
          <ListStateRow status="error" onRetry={onRetry} />
        ) : null}
        <ul role="list" className="divide-y divide-gray-alpha-400">
          {loops.map((openLoop) => {
            const line = (
              <>
                <span className={ROW_TITLE}>{openLoop.text}</span>
                <RowMeta
                  className="truncate"
                  source={openLoop.title}
                  time={formatRelativeTime(openLoop.at_utc_ms, nowMs)}
                />
              </>
            );

            return (
              <li
                key={`${openLoop.meeting_id}:${openLoop.at_utc_ms}:${openLoop.text}`}
              >
                {/* A promise whose meeting is gone still has to be read; it
                 * just has nothing to open. */}
                {openLoop.meeting_id === "" ? (
                  <div data-testid="overview-open-loop" className={ROW_BOX}>
                    {line}
                  </div>
                ) : (
                  <button
                    type="button"
                    data-testid="overview-open-loop"
                    data-meeting-id={openLoop.meeting_id}
                    aria-label={t("overview.needsYou.openMeeting", {
                      title: openLoop.title,
                    })}
                    onClick={() => onOpenMeeting(openLoop.meeting_id)}
                    className={ROW_LINK}
                  >
                    {line}
                  </button>
                )}
              </li>
            );
          })}

          {suggestions.map((entry) => (
            <li
              key={`${entry.loop_kind}:${entry.candidate_key}`}
              data-testid="overview-learning-suggestion"
              className={ROW_SPLIT}
            >
              <span className="min-w-0 flex-1">
                <span className={ROW_TITLE}>
                  {headline(entry.suggestion, t)}
                </span>
                {/* How often, and over how many days: the reason the question
                 * is being asked at all. The example quotes it used to carry
                 * were a second explanation of a sentence that already reads. */}
                <span className={cn(ROW_META, "tabular-nums")}>
                  {t("learningV2.feed.evidence", {
                    count: entry.evidence.occurrences,
                    days: entry.evidence.distinct_days,
                  })}
                </span>
              </span>
              <span className="flex shrink-0 items-center gap-1">
                {/* Advice is an observation: there is nothing to accept, only
                 * something to stop being told. */}
                {entry.suggestion.kind === "capture_advice" ? null : (
                  <Button
                    type="button"
                    size="xs"
                    onClick={() => onAnswerLearning(entry, "accepted")}
                  >
                    {t("learningV2.feed.accept")}
                  </Button>
                )}
                <Button
                  type="button"
                  size="xs"
                  variant="ghost"
                  onClick={() => onAnswerLearning(entry, "dismissed")}
                >
                  {t("learningV2.feed.dismiss")}
                </Button>
              </span>
            </li>
          ))}

          {updateWaiting === null ? null : (
            <li data-testid="overview-update" className={ROW_SPLIT}>
              <span className={cn(ROW_TITLE, "min-w-0 flex-1")}>
                {t("overview.update.available", {
                  latest: updateWaiting.latest_version ?? "",
                  current: updateWaiting.current_version,
                })}
              </span>
              <span className="flex shrink-0 items-center gap-1">
                {releaseUrl === null ? null : (
                  <Button
                    type="button"
                    variant="outline"
                    size="xs"
                    onClick={() => void openUrl(releaseUrl)}
                  >
                    {t("overview.update.view")}
                  </Button>
                )}
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label={t("overview.update.dismiss")}
                  onClick={onDismissUpdate}
                >
                  <X className="size-3.5" aria-hidden="true" />
                </Button>
              </span>
            </li>
          )}
        </ul>
      </SettingsCard>
    </div>
  );
};

const isSameLocalDay = (aMs: number, bMs: number): boolean => {
  const a = new Date(aMs);
  const b = new Date(bMs);
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
};

export interface RecentProps {
  receipts: FeedState<WorkflowRunReceipt>;
  onOpenMeeting: (meetingId: string) => void;
  onRetry: () => void;
  nowMs?: number;
}

/**
 * What Sona did without being asked, closed by default.
 *
 * The fact counts today's passes, so a reader knows whether opening this says
 * anything new — "0 today" over three older rows is the answer as often as
 * "3 today" is.
 */
export const Recent: React.FC<RecentProps> = ({
  receipts,
  onOpenMeeting,
  onRetry,
  nowMs = Date.now(),
}) => {
  const { t } = useTranslation();
  /* The list is effects, so a receipt with none does not reach it — the same
   * rule the loader pages by, applied where the row is drawn. */
  const entries =
    receipts.status === "loaded"
      ? receipts.entries
          .filter(workflowOutcomeHasEffect)
          .slice(0, OVERVIEW_RECEIPT_LIMIT)
      : [];
  const todayCount = entries.filter((receipt) =>
    isSameLocalDay(receipt.finished_at_utc_ms, nowMs),
  ).length;

  return (
    <SettingsDisclosure
      label={t("overview.recent.title", "Recent")}
      fact={
        receipts.status !== "loaded"
          ? undefined
          : entries.length === 0
            ? t("overview.recent.empty", "Nothing yet")
            : t("overview.recent.today", { count: todayCount })
      }
    >
      {receipts.status !== "loaded" ? (
        <ListStateRow status={receipts.status} onRetry={onRetry} />
      ) : entries.length === 0 ? (
        <p className={cn(ROW_BOX, EMPTY_LINE)}>
          {t(
            "overview.recent.emptyHint",
            "People, words and follow-ups Sona files after a meeting show up here.",
          )}
        </p>
      ) : (
        <ul role="list" className="divide-y divide-gray-alpha-400">
          {entries.map((receipt) => {
            const meetingId =
              receipt.jump_target?.kind === "meeting"
                ? receipt.jump_target.session_id
                : null;
            const line = (
              <>
                <span className={ROW_TITLE}>
                  {formatWorkflowOutcome(receipt, t)}
                </span>
                <RowMeta
                  source={t(WORKFLOW_NAME_KEY[receipt.workflow_id])}
                  time={formatRelativeTime(receipt.finished_at_utc_ms, nowMs)}
                />
              </>
            );

            return (
              <li key={receipt.id}>
                {/* A line with nothing to open is a line, not a dead button:
                 * skipping a detected meeting leaves no session behind. */}
                {meetingId === null ? (
                  <div
                    data-testid="overview-workflow-receipt"
                    className={ROW_BOX}
                  >
                    {line}
                  </div>
                ) : (
                  <button
                    type="button"
                    data-testid="overview-workflow-receipt"
                    data-meeting-id={meetingId}
                    onClick={() => onOpenMeeting(meetingId)}
                    className={ROW_LINK}
                  >
                    {line}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </SettingsDisclosure>
  );
};
