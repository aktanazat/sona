import React from "react";
import { useTranslation } from "react-i18next";
import type {
  MeetingLoopStatus,
  PersonCommitment,
  PersonOpenLoop,
} from "@/bindings";
import { CITATION_MARK } from "@/components/settings/meetings/review/Citations";
import { LoopStatusChip } from "@/components/settings/meetings/review/LoopRows";
import { Microlabel, SettingsSection } from "@/components/settings/rows";
import { cn } from "@/lib/cn";
import { formatEntryTimestamp } from "@/lib/utils/format";
import { groupByDirection } from "./loopDirection";

/* The mark that carries a ledger line back to the meeting it was said in.
 *
 * It is the citation mark NotesDocument owns — the same class string every
 * timestamp jump in the app renders in — because a jump back to a moment
 * should look the same everywhere. It cannot be `CitationJump` itself: a
 * person's ledger row carries a meeting id and a wall-clock time, never a
 * transcript segment or an in-meeting offset, so that component's props could
 * only be filled with nulls, which renders an unpressable dash. The meeting's
 * own name is in the row's Meta line below, and it is also this mark's
 * accessible name, so pressing it is never a guess. */
const MeetingMark: React.FC<{
  title: string;
  atUtcMs: number;
  onOpen: () => void;
}> = ({ title, atUtcMs, onOpen }) => {
  const { t } = useTranslation();
  const when = formatEntryTimestamp(atUtcMs);

  return (
    <button
      type="button"
      data-slot="ledger-jump"
      onClick={onOpen}
      aria-label={t("people.detail.openMeetingAt", { title, date: when })}
      title={t("people.detail.openMeetingAt", { title, date: when })}
      className={cn(CITATION_MARK, "ms-1 align-[0.05em]")}
    >
      {when}
    </button>
  );
};

/* One thing said in one meeting, in the shape both ledgers keep it.
 *
 * Open loops and commitments are two questions about the same record, which is
 * why they render through one section below rather than two copies of it. What
 * differs is the heading, the empty state, and whether a line carries the date
 * it was first raised — so those are fields, not a second component. */
interface LedgerRow {
  /** React key: the loop's own id, which is stable across re-reads. */
  key: string;
  text: string;
  /** The meeting the line was said in, as the link's own label. */
  title: string;
  meetingId: string;
  atUtcMs: number;
  /** When the loop was first raised, for a line that has outlived a meeting. */
  carriedSinceUtcMs: number | null;
  /** Where the loop stands now, read live from its store row. */
  status: MeetingLoopStatus;
  /** D27: this person has owed it for longer than a working week. */
  stale: boolean;
}

/* The sentence first, with the mark that goes back to the room it was said in,
 * then one Meta line: which meeting, where the loop stands, and how long it
 * has been standing there.
 *
 * Two groups inside one section, not two sections: "I owe" and "waiting on
 * them" are the same register read from opposite ends, and splitting the card
 * would make a page of four headings out of a page of two. A group with
 * nothing in it says nothing, and with both of them empty there is no section
 * at all: a heading over the sentence "no open loops" is a row spent saying
 * that the row has nothing to say. */
const LedgerSection: React.FC<{
  label: string;
  mine: LedgerRow[];
  waitingOn: LedgerRow[];
  waitingOnLabel: string;
  onOpenMeeting: (meetingId: string) => void;
}> = ({ label, mine, waitingOn, waitingOnLabel, onOpenMeeting }) => {
  const { t } = useTranslation();

  if (mine.length === 0 && waitingOn.length === 0) return null;

  const groups = [
    { key: "mine", heading: t("people.waitingOn.iOwe"), rows: mine },
    { key: "waitingOn", heading: waitingOnLabel, rows: waitingOn },
  ].filter((group) => group.rows.length > 0);

  return (
    <SettingsSection label={label}>
      {groups.map((group) => (
        <div key={group.key} className="flex flex-col">
          <h3 className="px-6 pt-3.5 pb-2">
            <Microlabel>{group.heading}</Microlabel>
          </h3>
          <ul className="divide-y divide-gray-alpha-400 border-t border-gray-alpha-400">
            {group.rows.map((row) => (
              <li key={row.key} className="flex flex-col gap-1.5 px-6 py-3.5">
                <p className="text-[14px] leading-[21px] text-gray-1000 text-pretty">
                  {row.text}
                  <MeetingMark
                    title={row.title}
                    atUtcMs={row.atUtcMs}
                    onOpen={() => onOpenMeeting(row.meetingId)}
                  />
                </p>
                <span className="snap-measured flex flex-wrap items-baseline gap-x-3 gap-y-1 text-[13px] leading-[18px] text-gray-900 tabular-nums">
                  <span className="min-w-0 truncate">{row.title}</span>
                  {/* "Open" under a heading that reads "Open loops" is the
                   * heading said twice. Only a status that contradicts the
                   * section it sits in - done, dropped, carried - is worth a
                   * word; the one a reader must act on is the stale line. */}
                  {row.status === "open" ? null : (
                    <LoopStatusChip status={row.status} />
                  )}
                  {row.stale ? (
                    <span
                      data-slot="loop-stale"
                      className="whitespace-nowrap text-red-900"
                    >
                      {t("people.waitingOn.stale")}
                    </span>
                  ) : null}
                  {/* When the loop was raised in the meeting the sentence
                   * already cites, "Open since" reprints that same clock
                   * time one line lower. It earns the line only when the
                   * loop has outlived the room it came from. */}
                  {row.carriedSinceUtcMs === null ||
                  row.carriedSinceUtcMs === row.atUtcMs ? null : (
                    <span>
                      {t("people.detail.carriedSince", {
                        date: formatEntryTimestamp(row.carriedSinceUtcMs),
                      })}
                    </span>
                  )}
                </span>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </SettingsSection>
  );
};

export const PersonOpenLoops: React.FC<{
  openLoops: PersonOpenLoop[];
  /** Whose page this is, for the "waiting on them" heading. */
  personName: string;
  onOpenMeeting: (meetingId: string) => void;
}> = ({ openLoops, personName, onOpenMeeting }) => {
  const { t } = useTranslation();
  const grouped = groupByDirection(openLoops);
  const asRow = (loop: PersonOpenLoop): LedgerRow => ({
    key: loop.loop_id,
    text: loop.text,
    title: loop.title,
    meetingId: loop.meeting_id,
    atUtcMs: loop.at_utc_ms,
    carriedSinceUtcMs: loop.carried_since_at_utc_ms,
    status: loop.status,
    stale: loop.waiting_on_stale,
  });

  return (
    <LedgerSection
      label={t("peopleV2.detail.openLoops")}
      mine={grouped.mine.map(asRow)}
      waitingOn={grouped.waitingOn.map(asRow)}
      waitingOnLabel={t("people.waitingOn.them", { name: personName })}
      onOpenMeeting={onOpenMeeting}
    />
  );
};

export const PersonCommitments: React.FC<{
  commitments: PersonCommitment[];
  personName: string;
  onOpenMeeting: (meetingId: string) => void;
}> = ({ commitments, personName, onOpenMeeting }) => {
  const { t } = useTranslation();
  const grouped = groupByDirection(commitments);
  const asRow = (commitment: PersonCommitment): LedgerRow => ({
    key: commitment.loop_id,
    text: commitment.text,
    title: commitment.title,
    meetingId: commitment.meeting_id,
    atUtcMs: commitment.at_utc_ms,
    carriedSinceUtcMs: null,
    status: commitment.status,
    stale: commitment.waiting_on_stale,
  });

  return (
    <LedgerSection
      label={t("people.detail.commitments")}
      mine={grouped.mine.map(asRow)}
      waitingOn={grouped.waitingOn.map(asRow)}
      waitingOnLabel={t("people.waitingOn.them", { name: personName })}
      onOpenMeeting={onOpenMeeting}
    />
  );
};
