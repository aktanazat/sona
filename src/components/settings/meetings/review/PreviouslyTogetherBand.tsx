import React, { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type MeetingPersonContextRow } from "@/bindings";
import { SettingsSection } from "@/components/settings/rows";
import { SpeakerBubbles } from "@/components/settings/meetings/home/SpeakerBubbles";
import { PersonDetailDialog } from "@/components/people/PersonDetailDialog";
import { formatEntryTimestamp } from "@/lib/utils/format";
import { usePeopleQuery } from "@/components/people/usePeopleQuery";

export interface PreviouslyTogetherRow {
  personId: string;
  displayName: string;
  meetingsCount: number;
  lastMeetingAtUtcMs: number;
  openLoop: string | null;
}

export const previouslyTogetherRows = (
  contextRows: readonly MeetingPersonContextRow[],
): PreviouslyTogetherRow[] =>
  contextRows.flatMap((row) => {
    if (row.last_prior_meeting === null) return [];
    return [
      {
        personId: row.person_id,
        displayName: row.display_name,
        meetingsCount: row.prior_meetings,
        lastMeetingAtUtcMs: row.last_prior_meeting.at_utc_ms,
        openLoop: row.top_open_loop?.text ?? null,
      },
    ];
  });

export const PreviouslyTogetherBandView: React.FC<{
  rows: PreviouslyTogetherRow[];
  onOpenPerson: (personId: string) => void;
}> = ({ rows, onOpenPerson }) => {
  const { t } = useTranslation();
  if (rows.length === 0) return null;

  return (
    <SettingsSection
      label={t("people.review.previouslyTogether")}
      className="mb-0"
    >
      <div
        data-slot="previously-together"
        className="divide-y divide-gray-alpha-400"
      >
        {rows.map((row) => (
          <button
            key={row.personId}
            type="button"
            className="flex w-full min-w-0 cursor-pointer items-start gap-4 px-6 py-3.5 text-start transition-colors hover:bg-hover focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none motion-reduce:transition-none"
            onClick={() => onOpenPerson(row.personId)}
          >
            <span className="min-w-0 flex-1">
              <SpeakerBubbles speakers={[row.displayName]} />
              {row.openLoop === null ? null : (
                <span className="mt-1.5 block truncate text-[14px] leading-[21px] text-gray-900">
                  {row.openLoop}
                </span>
              )}
            </span>
            <span className="flex flex-none flex-col items-end text-[13px] leading-[18px] tabular-nums text-gray-900">
              <span>
                {t("people.review.meetingsBefore", {
                  count: row.meetingsCount,
                })}
              </span>
              <span>
                {t("people.review.last", {
                  date: formatEntryTimestamp(row.lastMeetingAtUtcMs),
                })}
              </span>
            </span>
          </button>
        ))}
      </div>
    </SettingsSection>
  );
};

export const PreviouslyTogetherBand: React.FC<{ sessionId: string }> = ({
  sessionId,
}) => {
  const [selectedPersonId, setSelectedPersonId] = useState<string | null>(null);
  const loadContext = useCallback(async () => {
    const result = await commands.meetingPeopleContext(sessionId);
    if (result.status === "error") throw new Error(result.error);
    return previouslyTogetherRows(result.data.rows);
  }, [sessionId]);
  const { data } = usePeopleQuery(
    `meeting-people-context:${sessionId}`,
    loadContext,
  );

  return (
    <>
      <PreviouslyTogetherBandView
        rows={data ?? []}
        onOpenPerson={setSelectedPersonId}
      />
      <PersonDetailDialog
        personId={selectedPersonId}
        onPersonChange={setSelectedPersonId}
        onClose={() => setSelectedPersonId(null)}
      />
    </>
  );
};
