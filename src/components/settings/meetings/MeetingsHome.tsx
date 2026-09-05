import React, { useEffect, useState } from "react";
import { MoreHorizontal } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands } from "@/bindings";
import {
  Microlabel,
  PageTitle,
  SettingsPage,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import { MeetingSuggestionPreviews } from "./MeetingSuggestionPreviews";
import { PreMeetingCountdownCard } from "./PreMeetingCountdownCard";
import type {
  MeetingsHomeScreenActions,
  MeetingsHomeScreenModel,
} from "./meetingTypes";
import { MeetingsHistory } from "./home/MeetingsHistory";
import { MeetingsUpcoming } from "./home/MeetingsUpcoming";
import { MeetingsTrashDialog } from "./home/MeetingsTrash";

/* The shell's global shortcut and the command palette both land here and
 * expect the press to be under the cursor's next keystroke, so the button
 * carries an id rather than a ref through four hooks. */
const START_BUTTON_ID = "meeting-start-button";

type MeetingsHomeProps = MeetingsHomeScreenModel &
  MeetingsHomeScreenActions & {
    /** The shell's route setter, for the two rows that state a setting. */
    onOpenSettings?: () => void;
  };

/**
 * The calendar, and Record.
 *
 * Round 7 took the setup card off this page. One press has always been the
 * whole start flow, so the card around it was carrying three things that were
 * not the press: two capture chips that answered a question with one answer,
 * a retention sentence that only Settings can change, and an Import button
 * competing with Start for the same corner. The press is on the title line
 * now, the assurance is the one line under it, and the two operations nobody
 * performs weekly - importing a file, opening the recordings folder - sit in
 * the page's single "..." menu beside it, next to the bin that used to be a
 * section at the bottom of the page.
 *
 * What is left is the week ahead and the log of what already happened, which
 * is what a page called Meetings is for.
 */
export const MeetingsHome: React.FC<MeetingsHomeProps> = ({
  suggestions,
  recovery,
  meetings,
  loading,
  paging,
  hasMore,
  page,
  filter,
  error,
  sources,
  starting,
  focusStart,
  onStart,
  onImport,
  onStartSuggestion,
  onStartEvent,
  onOpenMeeting,
  onFinalizeRecovery,
  onDiscardRecovery,
  onFilterChange,
  onNextPage,
  onPreviousPage,
  onExportMeeting,
  onExportLedger,
  onDeleteMeeting,
  onRetry,
  onOpenSettings,
}) => {
  const { t } = useTranslation();
  const [trashOpen, setTrashOpen] = useState(false);

  useEffect(() => {
    if (!focusStart) return;
    document.getElementById(START_BUTTON_ID)?.focus();
  }, [focusStart]);

  /* Reveal in Finder, from the menu that owns the file-level verbs. The
   * palette runs the same command from the shell; both report the same way,
   * because a folder that did not open is not an error the page can fix. */
  const openRecordingsFolder = () => {
    void commands
      .openRecordingsFolder()
      .then((result) => {
        if (result.status !== "ok") toast.error(t("meetings.errors.operation"));
      })
      .catch(() => toast.error(t("meetings.errors.operation")));
  };

  return (
    <SettingsPage
      header={
        <div className="flex flex-col gap-2">
          <div className="flex items-center justify-between gap-4">
            <PageTitle>{t("meetings.title")}</PageTitle>
            <div className="flex flex-none items-center gap-1">
              <Button
                id={START_BUTTON_ID}
                type="button"
                onClick={onStart}
                disabled={starting}
              >
                {starting
                  ? t("meetings.start.starting")
                  : t("meetings.start.action")}
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("common.more")}
                  >
                    <MoreHorizontal aria-hidden="true" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="min-w-52">
                  <DropdownMenuItem onSelect={onImport}>
                    {t("meetings.import.action")}
                  </DropdownMenuItem>
                  <DropdownMenuItem onSelect={openRecordingsFolder}>
                    {t("meetings.actions.openRecordings")}
                  </DropdownMenuItem>
                  <DropdownMenuItem onSelect={() => setTrashOpen(true)}>
                    {t("meetings.trash.title")}
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
          {/* The one sentence this page keeps: what a press records, and what
           * it does not do. Every other line about capture was machinery
           * describing itself. */}
          <Microlabel>{t("meetings.start.assurance")}</Microlabel>
        </div>
      }
    >
      <PreMeetingCountdownCard
        sources={sources}
        starting={starting}
        onStartEvent={onStartEvent}
      />

      <MeetingSuggestionPreviews
        suggestions={suggestions}
        sources={sources}
        starting={starting}
        onStartSuggestion={onStartSuggestion}
      />

      <MeetingsUpcoming sources={sources} onOpenSettings={onOpenSettings} />

      <MeetingsHistory
        meetings={meetings}
        /* Meetings an interrupted launch left behind used to get their own
         * section above the log, which printed the same rows the log prints.
         * They are pinned to the top of the one list now, so each appears
         * exactly once, with its reason and its two verbs on it. */
        recovery={recovery}
        loading={loading}
        paging={paging}
        hasMore={hasMore}
        page={page}
        filter={filter}
        error={error}
        onOpenMeeting={onOpenMeeting}
        onFilterChange={onFilterChange}
        onNextPage={onNextPage}
        onPreviousPage={onPreviousPage}
        onExportMeeting={onExportMeeting}
        onExportLedger={onExportLedger}
        onDeleteMeeting={onDeleteMeeting}
        onDiscardMeeting={onDiscardRecovery}
        /* The same owner the pinned rows call: one command reprocesses an
         * interrupted meeting, wherever it is offered. */
        onReprocessMeeting={onFinalizeRecovery}
        onRetry={onRetry}
      />

      <MeetingsTrashDialog
        open={trashOpen}
        onOpenChange={setTrashOpen}
        onRestored={onRetry}
      />
    </SettingsPage>
  );
};
