import React, { useMemo, useState } from "react";
import { MoreHorizontal, Square } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { MeetingReviewSnapshot } from "@/bindings";
import {
  Microlabel,
  Notice,
  PageTitle,
  SettingsPage,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { Textarea } from "@/components/vg/textarea";
import { MeetingPhaseText } from "./MeetingStatus";
import { formatMeetingOffset } from "./meetingUtils";

/* Capture, while it runs.
 *
 * Round 7 emptied this page down to the four things a person in a meeting
 * looks at: what is being recorded, how long it has been going, the one press
 * that ends it, and the words arriving. Everything else was the capture
 * pipeline reporting on itself - a transcript offset, an ASR lag, a storage
 * row that said "Healthy", an inputs section repeating what the start gate
 * already cleared - and the words on screen are a better answer to "is this
 * working" than four measurements of the machinery that produces them. Pause,
 * Resume, the note box and Discard are behind the one menu on the title line,
 * because a control that ends or interrupts a recording is not something to
 * put under a reader's thumb while they are talking.
 *
 * The two lines that survive are the two the reader has to act on: a source
 * that dropped out mid-capture, and storage that stopped accepting writes.
 * Both are announced, because a person watching the call is not watching this
 * page. */

interface MeetingLiveProps {
  snapshot: MeetingReviewSnapshot;
  pendingAction: string | null;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
  onDiscard: () => void;
  onCreateNote: (body: string) => void;
}

export const MeetingLive: React.FC<MeetingLiveProps> = ({
  snapshot,
  pendingAction,
  onPause,
  onResume,
  onStop,
  onDiscard,
  onCreateNote,
}) => {
  const { t } = useTranslation();
  const [noteBody, setNoteBody] = useState("");
  const [noteOpen, setNoteOpen] = useState(false);
  const [discardOpen, setDiscardOpen] = useState(false);

  /* What has actually been heard, in the order it was heard. A removed
   * segment is one an editor took out; live, that is nearly always empty, and
   * an edited segment shows the edit rather than the raw pass. */
  const lines = useMemo(
    () => snapshot.transcript.filter((segment) => !segment.removed),
    [snapshot.transcript],
  );

  const systemAudio = snapshot.session.sources.find(
    (source) => source.source_kind === "system_audio",
  );
  const systemAudioLimited =
    systemAudio === undefined ||
    systemAudio.availability !== "available" ||
    systemAudio.health === "failed" ||
    systemAudio.health === "degraded";
  const storageAvailable = snapshot.session.storage === "available";
  const canPause = snapshot.session.allowed_actions.includes("pause");
  const canResume = snapshot.session.allowed_actions.includes("resume");
  const canStop = snapshot.session.allowed_actions.includes("stop");
  const canDiscard = snapshot.session.allowed_actions.includes("discard");
  const isPaused = snapshot.session.phase === "capturing_paused";
  const isMutating = pendingAction !== null;

  const addNote = () => {
    const body = noteBody.trim();
    if (body.length === 0) return;

    onCreateNote(body);
    setNoteBody("");
    setNoteOpen(false);
  };

  return (
    <SettingsPage
      header={
        <div className="flex items-center justify-between gap-4">
          <PageTitle className="min-w-0 truncate">
            {snapshot.session.title}
          </PageTitle>
          <div className="flex flex-none items-center gap-3">
            {/* The clock is the state while a capture is running, so the
             * phase word only appears when the state is not what the clock
             * implies: paused, stopping, or already processing. */}
            {snapshot.session.phase === "capturing_recording" ? null : (
              <MeetingPhaseText phase={snapshot.session.phase} />
            )}
            <span
              aria-label={t("meetings.live.elapsed")}
              className="text-[13px] leading-[18px] text-gray-900 tabular-nums"
            >
              {formatMeetingOffset(snapshot.session.elapsed_offset_ns)}
            </span>
            <Button
              type="button"
              onClick={onStop}
              disabled={!canStop || isMutating}
            >
              <Square aria-hidden="true" className="size-3" />
              {t("meetings.actions.stop")}
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
                {isPaused ? (
                  <DropdownMenuItem
                    disabled={!canResume || isMutating}
                    onSelect={onResume}
                  >
                    {t("meetings.actions.resume")}
                  </DropdownMenuItem>
                ) : (
                  <DropdownMenuItem
                    disabled={!canPause || isMutating}
                    onSelect={onPause}
                  >
                    {t("meetings.actions.pause")}
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem
                  disabled={isMutating}
                  onSelect={() => setNoteOpen(true)}
                >
                  {t("meetings.live.addNote")}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  variant="destructive"
                  disabled={!canDiscard || isMutating}
                  onSelect={() => setDiscardOpen(true)}
                >
                  {t("meetings.actions.discard")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>
      }
    >
      {storageAvailable ? null : (
        <Notice tone="danger" assertive>
          {t("meetings.live.storageUnavailable")}
        </Notice>
      )}

      {systemAudioLimited ? (
        <Notice tone="info">{t("meetings.live.microphoneOnlyPartial")}</Notice>
      ) : snapshot.session.capture_completeness === "partial" ? (
        <Notice tone="info">{t("meetings.live.partialCapture")}</Notice>
      ) : null}

      {lines.length === 0 ? (
        <p className="text-[13px] leading-5 text-gray-800">
          {t("meetings.live.transcriptEmpty")}
        </p>
      ) : (
        <ol
          data-slot="live-transcript"
          aria-label={t("meetings.live.transcript")}
          className="flex flex-col gap-2"
        >
          {lines.map((segment) => (
            <li
              key={segment.base.segment_id}
              className="flex items-baseline gap-5"
            >
              <Microlabel className="w-[62px] flex-none text-end tabular-nums">
                {formatMeetingOffset(segment.base.start_offset_ns)}
              </Microlabel>
              <span className="min-w-0 flex-1 text-[14px] leading-[21px] text-gray-1000">
                {segment.replacement_text ?? segment.base.text}
              </span>
            </li>
          ))}
        </ol>
      )}

      <Dialog open={noteOpen} onOpenChange={setNoteOpen}>
        <DialogContent>
          <DialogHeader>
            {/* The note is anchored at the clock when it is saved, which is
             * why nothing here asks for a time. */}
            <DialogTitle>{t("meetings.live.manualNote")}</DialogTitle>
          </DialogHeader>
          <Textarea
            value={noteBody}
            onChange={(event) => setNoteBody(event.target.value)}
            aria-label={t("meetings.live.manualNote")}
            placeholder={t("meetings.live.notePlaceholder")}
            disabled={isMutating}
            rows={4}
            className="resize-none"
          />
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setNoteOpen(false)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              type="button"
              onClick={addNote}
              disabled={noteBody.trim().length === 0 || isMutating}
            >
              {t("meetings.live.addNote")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={discardOpen} onOpenChange={setDiscardOpen}>
        <DialogContent showCloseButton={false}>
          <DialogHeader>
            <DialogTitle>{t("meetings.discard.liveTitle")}</DialogTitle>
            <DialogDescription>
              {t("meetings.discard.explainsData")}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setDiscardOpen(false)}
            >
              {t("meetings.discard.keepRecording")}
            </Button>
            <Button
              type="button"
              variant="destructive"
              onClick={() => {
                setDiscardOpen(false);
                onDiscard();
              }}
            >
              {t("meetings.discard.stopAndDiscard")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </SettingsPage>
  );
};
