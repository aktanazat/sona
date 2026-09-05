import React from "react";
import { useTranslation } from "react-i18next";
import type {
  MeetingLoopRow,
  MeetingReviewSnapshot,
  PersonListEntry,
} from "@/bindings";
import {
  Microlabel,
  Notice,
  SettingsSurface,
} from "@/components/settings/rows";
import { LedgerReceiptRow } from "./review/LedgerReceiptRow";
import { LoopRows, type LoopChange } from "./review/LoopRows";
import { formatMeetingOffset } from "./meetingUtils";
import {
  currentLedger,
  LEDGER_OUTCOME,
  type LedgerOutcome,
} from "./meetingLedger";

/* Where did we land, and what did we leave open.
 *
 * Adapted from the where-did-we-land skill by gnurio (MIT licence,
 * https://github.com/gnurio/where-did-we-land): the state vocabulary, the
 * receipt-beside-every-state discipline and the register set are upstream's.
 * See NOTICE.
 *
 * The split this surface exists to show: a count is measured, a state is
 * inferred, and an inferred state is only worth reading next to the quote it
 * was read from. So every row carries its receipt, and every receipt carries
 * the citation jump — the same control the rest of the review uses, because a
 * citation is a jump wherever it appears.
 *
 * One card, hairline blocks, compact measurements. No tally line: counting
 * the commitments and the open loops above lists that print every one of them
 * was the same number said twice, and "Threads settled 0/1" was a score for a
 * conversation nobody was scoring. A thread still waiting on an answer says
 * so on its own row. */

/* A landed thread prints no state word: the row, its quote and the fact that
 * nothing is asked of the reader are the answer. The two states somebody
 * still has to do something about say so, in the page's two status colours —
 * colour is the second channel, the word says it either way. */
const UNSETTLED_CLASSES = {
  open: "text-accent-strong",
  dropped: "text-red-900",
} as const satisfies Record<Exclude<LedgerOutcome, "landed">, string>;

const COLUMN_CLASSES =
  "pb-1.5 pe-3 text-start text-[13px] leading-[18px] font-normal text-gray-900";

const CELL_CLASSES = "py-1.5 pe-3 align-top";

const offsetOf = (milliseconds: number) =>
  formatMeetingOffset(milliseconds * 1_000_000);

export interface MeetingLedgerSectionProps {
  snapshot: MeetingReviewSnapshot;
  busy: boolean;
  /** Actionable rows for this meeting, or null until the first read lands. */
  loops: MeetingLoopRow[] | null;
  /** Everybody who could own a loop, for the owner picker. */
  people: PersonListEntry[];
  onJumpToSegment: (segmentId: string) => void;
  onLoopChange: (row: MeetingLoopRow, change: LoopChange) => void;
}

export const MeetingLedgerSection: React.FC<MeetingLedgerSectionProps> = ({
  snapshot,
  busy,
  loops,
  people,
  onJumpToSegment,
  onLoopChange,
}) => {
  const { t } = useTranslation();
  const ledger = currentLedger(snapshot.artifacts);

  if (ledger === null) {
    return (
      <SettingsSurface>
        <p className="px-6 py-6 text-[13px] leading-5 text-gray-800">
          {t("meetings.ledger.emptyDescription")}
        </p>
      </SettingsSurface>
    );
  }

  return (
    /* No label over this surface either: the tab reading "Ledger" named it,
     * and the two verbs that sat on that label line — draft a follow-up,
     * export the page — are rows of the review page's one menu now. */
    <SettingsSurface>
      {/* The headline is what this document says, so it is the first thing
       * read on it and it is set as a paragraph, not as a row of body text
       * under a row of counts. */}
      <p className="px-6 py-5 text-[16px] leading-[25px] text-pretty text-gray-1000">
        {ledger.headline}
      </p>

      <LedgerBlock label={t("meetings.ledger.threads")}>
        <ul
          role="list"
          aria-label={t("meetings.ledger.threads")}
          className="flex flex-col gap-4"
        >
          {ledger.threads.map((thread, index) => {
            const outcome = LEDGER_OUTCOME[thread.state];
            return (
              <li key={`thread:${index}`} className="flex flex-col gap-1">
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
                  <span className="flex min-w-0 items-baseline gap-2">
                    <span className="text-[14px] leading-[21px] font-medium text-gray-1000">
                      {thread.topic}
                    </span>
                    {thread.owner ? (
                      <span className="text-[13px] leading-[18px] text-gray-900">
                        {thread.owner}
                      </span>
                    ) : null}
                    {thread.substantive ? null : (
                      <Microlabel>
                        {t("meetings.ledger.asideThread")}
                      </Microlabel>
                    )}
                  </span>
                  {outcome === "landed" ? null : (
                    <span
                      className={`flex-none text-[13px] leading-[18px] whitespace-nowrap ${UNSETTLED_CLASSES[outcome]}`}
                    >
                      {t(`meetings.ledger.states.${thread.state}`)}
                    </span>
                  )}
                </div>
                <LedgerReceiptRow
                  quote={thread.receipt.quote}
                  speaker={thread.receipt.speaker}
                  atMs={thread.receipt.t_ms}
                  citations={thread.receipt.citations}
                  onJumpToSegment={onJumpToSegment}
                />
              </li>
            );
          })}
        </ul>
      </LedgerBlock>

      {/* Two registers, one control set. Both are things somebody still has to
       * do, so they read and act the same way; only the heading and the
       * absence line differ. Until the first read lands there is nothing to
       * act on, and a spinner over four rows is worse than the wait. */}
      <LedgerBlock label={t("meetings.ledger.openLoops")}>
        <LoopRows
          rows={
            loops === null ? [] : loops.filter((row) => row.kind === "loop")
          }
          people={people}
          disabled={busy || loops === null}
          emptyText={t("meetings.ledger.noOpenLoops")}
          onChange={onLoopChange}
          onJumpToSegment={onJumpToSegment}
        />
      </LedgerBlock>

      <LedgerBlock label={t("meetings.ledger.commitments")}>
        <LoopRows
          rows={
            loops === null
              ? []
              : loops.filter((row) => row.kind === "commitment")
          }
          people={people}
          disabled={busy || loops === null}
          emptyText={t("meetings.ledger.noCommitments")}
          onChange={onLoopChange}
          onJumpToSegment={onJumpToSegment}
        />
      </LedgerBlock>

      <LedgerBlock label={t("meetings.ledger.stances")}>
        {ledger.stances.length === 0 ? (
          <Notice tone="muted" live={false}>
            {t("meetings.ledger.noStances")}
          </Notice>
        ) : (
          <table className="w-full text-[14px] leading-[21px] text-gray-900">
            <thead>
              <tr>
                <th scope="col" className={COLUMN_CLASSES}>
                  {t("meetings.ledger.columnAt")}
                </th>
                <th scope="col" className={COLUMN_CLASSES}>
                  {t("meetings.ledger.columnDirection")}
                </th>
                <th scope="col" className={COLUMN_CLASSES}>
                  {t("meetings.ledger.columnWhat")}
                </th>
                <th scope="col" className={COLUMN_CLASSES}>
                  {t("meetings.ledger.columnTaken")}
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-alpha-400">
              {ledger.stances.map((stance, index) => (
                <tr key={`stance:${index}`}>
                  <td
                    className={`${CELL_CLASSES} text-[13px] leading-[18px] tabular-nums whitespace-nowrap text-gray-900`}
                  >
                    {offsetOf(stance.at_ms)}
                  </td>
                  <td
                    className={`${CELL_CLASSES} font-medium whitespace-nowrap text-gray-1000`}
                  >
                    {stance.to === null
                      ? stance.from
                      : stance.from + " → " + stance.to}
                  </td>
                  <td className={`${CELL_CLASSES} text-gray-1000`}>
                    {stance.what}
                  </td>
                  <td className={CELL_CLASSES}>{stance.note ?? ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </LedgerBlock>

      <LedgerBlock label={t("meetings.ledger.trust")}>
        <ul role="list" className="flex flex-col gap-1.5">
          <li className="text-[14px] leading-[21px] text-pretty text-gray-900">
            {t("meetings.ledger.trustMeasured")}
          </li>
          {/* What was thrown away for failing its own receipt check. It used
           * to be half of a "Receipts verified" tally at the top of the page,
           * which printed a word for the ordinary case nobody has to read.
           * Kept here, in the block about what to trust, and only when
           * something was actually dropped. */}
          {ledger.receipts.status === "verified" ? null : (
            <li className="text-[14px] leading-[21px] text-pretty text-accent-strong">
              {t("meetings.ledger.receiptsDegraded", {
                threads: ledger.receipts.dropped_threads,
                commitments: ledger.receipts.dropped_commitments,
              })}
            </li>
          )}
          {ledger.caveats.map((caveat, index) => (
            <li
              key={`caveat:${index}`}
              className="text-[14px] leading-[21px] text-pretty text-gray-900"
            >
              {caveat}
            </li>
          ))}
        </ul>
      </LedgerBlock>
    </SettingsSurface>
  );
};

interface LedgerBlockProps {
  label: string;
  children: React.ReactNode;
}

/** One register of the ledger: a microlabel over its rows, on a hairline. */
const LedgerBlock: React.FC<LedgerBlockProps> = ({ label, children }) => (
  <div className="flex flex-col gap-2 px-6 py-4">
    <h3>
      <Microlabel>{label}</Microlabel>
    </h3>
    {children}
  </div>
);
