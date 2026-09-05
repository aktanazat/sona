import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import type { PersonMeetingLink } from "@/bindings";
import { Microlabel, SettingsSection } from "@/components/settings/rows";
import { MeetingSummaryRow } from "@/components/settings/meetings/home/MeetingCard";
import { DropdownMenuItem } from "@/components/vg/dropdown-menu";
import { formatEntryTimestamp } from "@/lib/utils/format";
import { PeopleConfirmDialog } from "./PeopleConfirmDialog";

interface PersonMeetingsProps {
  links: PersonMeetingLink[];
  pending: boolean;
  onConfirm: (link: PersonMeetingLink) => void;
  onUnlink: (link: PersonMeetingLink) => void;
}

export const PersonMeetings: React.FC<PersonMeetingsProps> = ({
  links,
  pending,
  onConfirm,
  onUnlink,
}) => {
  const { t } = useTranslation();
  const [unlinking, setUnlinking] = useState<PersonMeetingLink | null>(null);

  /* No meetings together, no section. The row that used to say so was the
   * only thing under the heading, and a heading with one sentence of nothing
   * under it is two rows spent on an absence. */
  if (links.length === 0) return null;

  return (
    <SettingsSection label={t("people.detail.meetingsTogether")}>
      <ul className="divide-y divide-gray-alpha-400">
        {links.map((link) => {
          const suggested = link.confidence === "suggested";
          /* Where the link came from is the page's evidence section now; what
           * stays on the row is the one word that asks the reader for a
           * decision, and it is a word, not a badge. */
          const metadata = [
            ...(suggested
              ? [
                  <span key="suggested" className="text-accent-strong">
                    {t("people.source.suggested")}
                  </span>,
                ]
              : []),
            ...(link.meeting.series_number < 2
              ? []
              : [
                  <Microlabel key="series" className="tabular-nums">
                    {t("people.detail.series", {
                      number: link.meeting.series_number,
                    })}
                  </Microlabel>,
                ]),
            <span key="timestamp" className="tabular-nums">
              {formatEntryTimestamp(link.meeting.at_utc_ms)}
            </span>,
          ];

          return (
            <MeetingSummaryRow
              key={link.meeting.id}
              data-slot="person-meeting"
              className="px-6 py-3.5"
              title={link.meeting.title}
              headline={link.meeting.headline}
              actionsLabel={t("common.more")}
              metadata={metadata}
              actions={
                <>
                  {suggested ? (
                    <DropdownMenuItem
                      disabled={pending}
                      onSelect={() => onConfirm(link)}
                    >
                      {t("people.list.confirm")}
                    </DropdownMenuItem>
                  ) : null}
                  <DropdownMenuItem
                    disabled={pending}
                    variant={suggested ? "default" : "destructive"}
                    onSelect={() => {
                      if (suggested) onUnlink(link);
                      else setUnlinking(link);
                    }}
                  >
                    {suggested
                      ? t("people.list.dismiss")
                      : t("people.detail.unlink")}
                  </DropdownMenuItem>
                </>
              }
            />
          );
        })}
      </ul>

      <PeopleConfirmDialog
        open={unlinking !== null}
        onOpenChange={(open) => {
          if (!open) setUnlinking(null);
        }}
        title={t("people.detail.unlinkTitle")}
        description={t("people.detail.unlinkDescription", {
          meeting: unlinking?.meeting.title ?? "",
        })}
        confirmLabel={t("people.detail.unlink")}
        pending={pending}
        destructive
        onConfirm={() => {
          if (unlinking !== null) onUnlink(unlinking);
          setUnlinking(null);
        }}
      />
    </SettingsSection>
  );
};
