import React, { useEffect, useState } from "react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { commands } from "@/bindings";
import { Button } from "@/components/vg/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import {
  followUpDraftText,
  meetingFollowUpDraft,
  type MeetingFollowUpDraft,
} from "./followUpDraft";

/* D26: one press turns the record into a message.
 *
 * The sheet shows the draft before anything is copied, because the whole point
 * is that a person reads it and sends it in their own words. It also says
 * plainly where the draft came from: a message an engine wrote is a rewrite of
 * the record and worth checking, and a draft assembled from the record is
 * verbatim and worth trusting. Those are different things to hand somebody,
 * so the sheet does not pretend they are the same.
 *
 * The press itself is one row of the review page's menu, so this is the sheet
 * and nothing else: the menu owns whether it is open, and asking to open it
 * is what asks the backend to write the draft. */

interface FollowUpDraftDialogProps {
  sessionId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export const FollowUpDraftDialog: React.FC<FollowUpDraftDialogProps> = ({
  sessionId,
  open,
  onOpenChange,
}) => {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<MeetingFollowUpDraft | null>(null);

  /* Opening the sheet is the request. A draft is a read of one revision, so
   * it is written afresh every time the sheet opens and dropped when it
   * closes rather than cached into a later press. */
  useEffect(() => {
    if (!open) {
      setDraft(null);
      return;
    }
    let disposed = false;
    void (async () => {
      try {
        const next = await meetingFollowUpDraft(crypto.randomUUID(), sessionId);
        if (!disposed) setDraft(next);
      } catch {
        if (disposed) return;
        toast.error(t("meetings.followUp.failed"));
        onOpenChange(false);
      }
    })();
    return () => {
      disposed = true;
    };
  }, [open, sessionId, t, onOpenChange]);

  const body = draft === null ? "" : followUpDraftText(draft, t);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(body);
      onOpenChange(false);
      toast.success(t("meetings.followUp.copied"));
    } catch {
      toast.error(t("meetings.followUp.copyFailed"));
    }
  };

  /* The draft, addressed. The backend owns the recipients, the subject, the
   * encoding and the length bound; the words and the clipboard are this side's,
   * exactly as they are for Copy. */
  const openInMail = async () => {
    if (draft === null) return;
    try {
      const mail = await commands.meetingFollowUpMail({
        session_id: draft.session_id,
        body,
        over_bound_note: t("meetings.followUp.mailClipboardNote"),
      });
      if (mail.status === "error") {
        toast.error(t("meetings.followUp.mailFailed"));
        return;
      }
      if (mail.data.body === "clipboard") {
        await navigator.clipboard.writeText(body);
      }
      await openUrl(mail.data.url);
      onOpenChange(false);
      toast.success(
        mail.data.body === "clipboard"
          ? t("meetings.followUp.mailCopied")
          : t("meetings.followUp.mailOpened"),
      );
    } catch {
      toast.error(t("meetings.followUp.mailFailed"));
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("meetings.followUp.title")}</DialogTitle>
          <DialogDescription>
            {draft === null
              ? t("meetings.followUp.drafting")
              : draft.source === "generated"
                ? t("meetings.followUp.fromEngine")
                : t("meetings.followUp.fromRecord")}
          </DialogDescription>
        </DialogHeader>
        {/* The draft owns the scroll, so the sheet's own footer never leaves
         * the window on a long meeting. */}
        <div className="max-h-64 overflow-y-auto">
          <p className="text-[14px] leading-[21px] whitespace-pre-wrap text-pretty text-gray-1000">
            {body}
          </p>
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {t("common.cancel")}
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={draft === null}
            onClick={() => void copy()}
          >
            {t("meetings.followUp.copy")}
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={draft === null}
            onClick={() => void openInMail()}
          >
            {t("meetings.followUp.openInMail")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};
