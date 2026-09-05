import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands, type MeetingTrashEntry } from "@/bindings";
import { Microlabel, SETTINGS_SURFACE } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { formatEntryTimestamp } from "@/lib/utils/format";

/* The undo bin, behind the one menu on the page that deletes.
 *
 * It belongs to Meetings home rather than Settings because this is the page
 * the Delete action lives on, and a bin somewhere else would be a second place
 * to look for the meeting you just lost. Round 6 kept it as a section at the
 * bottom of the page that rendered nothing when empty, which meant the page
 * grew a whole extra list every time somebody deleted a meeting. Round 7 put
 * it where an operation nobody performs weekly belongs: in the "..." menu, and
 * on screen only while it is being read.
 *
 * Self-fetching, and only while open: the controller owns live meeting state,
 * and a deleted meeting is not one. */

const DAY_MS = 24 * 60 * 60 * 1_000;

interface MeetingsTrashDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Refreshes the history list, so a restored meeting appears in it. */
  onRestored: () => void;
}

export const MeetingsTrashDialog: React.FC<MeetingsTrashDialogProps> = ({
  open,
  onOpenChange,
  onRestored,
}) => {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<MeetingTrashEntry[]>([]);
  const [restoring, setRestoring] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const result = await commands.meetingTrashList();
    setEntries(result.status === "ok" ? result.data : []);
  }, []);

  /* Read on open, not on mount: the bin is now a dialog, so the page no
   * longer spends a command on a list nobody asked to see. */
  useEffect(() => {
    if (!open) return;
    void refresh();
  }, [open, refresh]);

  const restore = async (entry: MeetingTrashEntry) => {
    setRestoring(entry.job_id);
    try {
      const result = await commands.meetingTrashRestore(entry.job_id);
      if (result.status === "error") {
        toast.error(t("meetings.trash.restoreFailed"));
        return;
      }
      toast.success(t("meetings.trash.restored", { title: result.data.title }));
      await refresh();
      onRestored();
    } catch {
      toast.error(t("meetings.trash.restoreFailed"));
    } finally {
      setRestoring(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("meetings.trash.title")}</DialogTitle>
        </DialogHeader>

        {entries.length === 0 ? (
          <p className="text-[13px] leading-5 text-gray-800">
            {t("meetings.trash.empty")}
          </p>
        ) : (
          <ul
            aria-label={t("meetings.trash.title")}
            className={SETTINGS_SURFACE}
            data-slot="meetings-trash"
          >
            {entries.map((entry) => {
              const days = Math.ceil(
                (entry.expires_at_utc_ms - Date.now()) / DAY_MS,
              );
              return (
                <li
                  key={entry.job_id}
                  className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 px-6 py-3.5"
                >
                  <span className="flex min-w-0 flex-col gap-1">
                    <span className="truncate text-[13px] leading-5 font-medium text-gray-1000">
                      {entry.title}
                    </span>
                    <Microlabel className="snap-measured tabular-nums">
                      {days > 0
                        ? t("meetings.trash.expires", {
                            deleted: formatEntryTimestamp(
                              entry.deleted_at_utc_ms,
                            ),
                            count: days,
                          })
                        : t("meetings.trash.expiresToday", {
                            deleted: formatEntryTimestamp(
                              entry.deleted_at_utc_ms,
                            ),
                          })}
                    </Microlabel>
                  </span>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    aria-label={t("meetings.trash.restoreItem", {
                      title: entry.title,
                    })}
                    disabled={restoring !== null}
                    onClick={() => void restore(entry)}
                  >
                    {t("meetings.trash.restore")}
                  </Button>
                </li>
              );
            })}
          </ul>
        )}
      </DialogContent>
    </Dialog>
  );
};
